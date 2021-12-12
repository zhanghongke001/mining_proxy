use std::sync::Arc;

use anyhow::Result;
use bytes::BytesMut;
use clap::{crate_name, crate_version};
use log::info;
use native_tls::Identity;
use tokio::{
    fs::File,
    io::AsyncReadExt,
    sync::{broadcast, mpsc, RwLock},
};

mod client;
mod mine;
mod protocol;
mod state;
mod util;

use util::{config, get_app_command_matches, logger};

use crate::{
    client::{tcp::accept_tcp, tls::accept_tcp_with_tls},
    mine::Mine,
    state::State,
    util::hex_to_hashrate,
};

const DEVFEE: bool = true;
const FEE: f64 = 0.01;

#[tokio::main]
async fn main() -> Result<()> {
    hex_to_hashrate("0x1a5c3611")?;

    let matches = get_app_command_matches().await?;
    let config_file_name = matches.value_of("config").unwrap_or("default.yaml");
    let config = config::Settings::new(config_file_name)?;
    logger::init(crate_name!(), config.log_path.clone(), config.log_level)?;
    if config.pool_ssl_address.is_empty() && config.pool_tcp_address.is_empty() {
        info!("❎ TLS矿池或TCP矿池必须启动其中的一个");
        panic!();
    };

    let mut p12 = File::open(config.p12_path.clone())
        .await
        .expect("证书路径错误");

    let mut buffer = BytesMut::with_capacity(10240);
    let read_key_len = p12.read_buf(&mut buffer).await?;
    info!("✅ 证书读取成功，证书字节数为: {}", read_key_len);
    let cert = Identity::from_pkcs12(&buffer[0..read_key_len], config.p12_pass.clone().as_str())?;

    info!("✅ config init success!");
    info!("✅ {}, 版本:{}", crate_name!(), crate_version!());
    // 分配任务给矿机channel
    let (job_send, _) = broadcast::channel::<String>(1);
    // 分配任务给矿机channel
    //let (job_send, jobs_recv) = mpsc::channel::<String>(50);

    // 中转抽水费用
    let mine = Mine::new(config.clone()).await?;
    let (proxy_fee_sender, proxy_fee_recver) = mpsc::channel::<String>(50);

    // 开发者费用
    let (fee_tx, _) = mpsc::channel::<String>(50);
    // let develop_account = "0x98be5c44d574b96b320dffb0ccff116bda433b8e".to_string();
    // let develop_mine = mine::develop::Mine::new(config.clone(), develop_account).await?;

    // 当前中转总报告算力。Arc<> Or atom 变量
    let state = Arc::new(RwLock::new(State::new()));

    let _ = tokio::join!(
        accept_tcp(
            state.clone(),
            config.clone(),
            job_send.clone(),
            proxy_fee_sender.clone(),
            fee_tx.clone()
        ),
        accept_tcp_with_tls(
            state.clone(),
            config.clone(),
            job_send.clone(),
            proxy_fee_sender.clone(),
            fee_tx.clone(),
            cert
        ),
        mine.accept(
            state.clone(),
            job_send,
            proxy_fee_sender.clone(),
            proxy_fee_recver
        ),
        //develop_mine.accept_tcp_with_tls(fee_tx.clone(), fee_rx),
    );

    Ok(())
}
