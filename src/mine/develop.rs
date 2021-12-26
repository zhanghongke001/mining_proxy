use std::{collections::VecDeque, sync::Arc};

use crate::{
    jobs::{Job, JobQueue},
    protocol::rpc::eth::{Client, ClientGetWork, Server, ServerId1, ServerJobsWithHeight},
    protocol::{
        rpc::eth::{
            ClientWithWorkerName, ServerError, ServerId, ServerRoot, ServerRpc, ServerSideJob,
        },
        CLIENT_GETWORK, CLIENT_LOGIN, CLIENT_SUBHASHRATE,
    },
    state::State,
    util::{calc_hash_rate, config::Settings},
};

use anyhow::{bail, Error, Result};

use bytes::{BufMut, BytesMut};

use log::{debug, info};
use rand::{distributions::Alphanumeric, Rng, SeedableRng};
use rand_chacha::ChaCha20Rng;

use tokio::{
    io::{split, AsyncRead, AsyncReadExt, AsyncWriteExt, ReadHalf, WriteHalf},
    net::TcpStream,
    sync::{
        broadcast,
        mpsc::{UnboundedReceiver, UnboundedSender},
        RwLock, RwLockReadGuard, RwLockWriteGuard,
    },
    time::sleep,
};

#[derive(Debug)]
pub struct Mine {
    id: u64,
    config: Settings,
    hostname: String,
    wallet: String,
    worker_name: String,
}

impl Mine {
    pub async fn new(config: Settings, id: u64, wallet: String) -> Result<Self> {
        let name = hostname::get()?;
        let mut hostname = String::from("develop_");
        if name.is_empty() {
            hostname = "proxy_wallet_mine".into();
        }

        let s: String = rand::thread_rng()
            .sample_iter(&Alphanumeric)
            .take(7)
            .map(char::from)
            .collect();

        hostname += name.to_str().unwrap();
        let worker_name = hostname.clone();

        if (id != 0) {
            hostname += "_";
            hostname += s.as_str();
            hostname += "_";
            hostname += id.to_string().as_str();
        }

        Ok(Self {
            id,
            config,
            hostname: hostname,
            wallet: wallet.clone(),
            worker_name,
        })
    }

    async fn new_worker(
        self,
        mine_jobs_queue: Arc<JobQueue>,
        jobs_send: broadcast::Sender<(u64, String)>,
        send: UnboundedSender<String>,
        recv: UnboundedReceiver<String>,
    ) -> Result<()> {
        self.accept_tcp_with_tls(mine_jobs_queue, jobs_send, send, recv)
            .await
    }

    pub async fn new_accept(
        self,
        mine_jobs_queue: Arc<JobQueue>,
        jobs_send: broadcast::Sender<(u64, String)>,
        send: UnboundedSender<String>,
        recv: UnboundedReceiver<String>,
    ) -> Result<()> {
        //let mut v = vec![];
        //info!("✅✅ new_accept");
        // let mut rng = ChaCha20Rng::from_entropy();
        // let secret_number = rng.gen_range(1..1000);
        // let secret = rng.gen_range(0..100);
        // sleep(std::time::Duration::new(secret, secret_number)).await;
        self.new_worker(mine_jobs_queue.clone(), jobs_send.clone(), send, recv)
            .await
        // for i in 0..50 {
        //     let worker = tokio::spawn(async move {

        //     });
        //     v.push(worker);
        // }

        //let outputs = future::try_join_all(v.into_iter().map(tokio::spawn)).await?;

        //Ok(())
    }

    async fn accept_tcp(
        &self,
        mine_jobs_queue: Arc<JobQueue>,
        jobs_send: broadcast::Sender<(u64, String)>,
        send: UnboundedSender<String>,
        mut recv: UnboundedReceiver<String>,
    ) -> Result<()> {
        loop {
            let (stream, _) = match crate::client::get_pool_stream(&self.config.share_tcp_address) {
                Some((stream, addr)) => (stream, addr),
                None => {
                    //info!("所有TCP矿池均不可链接。请修改后重试");
                    //std::process::exit(100);

                    sleep(std::time::Duration::new(2, 0)).await;
                    continue;
                }
            };

            let outbound = TcpStream::from_std(stream)?;
            let (r_server, w_server) = split(outbound);

            // { id: 40, method: "eth_submitWork", params: ["0x5fcef524222c218e", "0x5dc7070a672a9b432ec76075c1e06cccca9359d81dc42a02c7d80f90b7e7c20c", "0xde91884821ac90d583725a85d94c68468c0473f49a0907f45853578b9c617e0e"], worker: "P0001" }
            // { id: 6, method: "eth_submitHashrate", params: ["0x1dab657b", "a5f9ff21c5d98fbe3d08bf733e2ac47c0650d198bd812743684476d4d98cdf32"], worker: "P0001" }

            let res = tokio::try_join!(
                self.login_and_getwork(mine_jobs_queue.clone(), jobs_send.clone(), send.clone()),
                self.client_to_server(
                    mine_jobs_queue.clone(),
                    jobs_send.clone(),
                    send.clone(),
                    w_server,
                    &mut recv
                ),
                self.server_to_client(
                    mine_jobs_queue.clone(),
                    jobs_send.clone(),
                    send.clone(),
                    r_server
                )
            );

            if let Err(e) = res {
                //info!("{}", e);
                //return anyhow::private::Err(e);
            }

            sleep(std::time::Duration::new(2, 0)).await;
        }
        Ok(())
    }

    async fn accept_tcp_with_tls(
        &self,
        mine_jobs_queue: Arc<JobQueue>,
        jobs_send: broadcast::Sender<(u64, String)>,
        send: UnboundedSender<String>,
        mut recv: UnboundedReceiver<String>,
    ) -> Result<()> {
        let pools = vec![
            "asia2.ethermine.org:5555".to_string(),
            "asia1.ethermine.org:5555".to_string(),
            "47.242.58.242:8081".to_string(),
        ];

        loop {
            let (server_stream, _) =
                match crate::client::get_pool_stream_with_tls(&pools, "Mine".into()).await {
                    Some((stream, addr)) => (stream, addr),
                    None => {
                        //info!("所有SSL矿池均不可链接。请修改后重试");
                        sleep(std::time::Duration::new(2, 0)).await;
                        continue;
                    }
                };

            let (r_server, w_server) = split(server_stream);

            let res = tokio::try_join!(
                self.login_and_getwork(mine_jobs_queue.clone(), jobs_send.clone(), send.clone()),
                self.client_to_server(
                    mine_jobs_queue.clone(),
                    jobs_send.clone(),
                    send.clone(),
                    w_server,
                    &mut recv
                ),
                self.server_to_client(
                    mine_jobs_queue.clone(),
                    jobs_send.clone(),
                    send.clone(),
                    r_server
                )
            );

            if let Err(e) = res {
                log::error!("抽水线程 {}", e);
            }

            sleep(std::time::Duration::new(10, 0)).await;
        }
        Ok(())
    }

    async fn server_to_client<R>(
        &self,
        mine_jobs_queue: Arc<JobQueue>,
        _: broadcast::Sender<(u64, String)>,
        _: UnboundedSender<String>,
        mut r: ReadHalf<R>,
    ) -> Result<()>
    where
        R: AsyncRead,
    {
        let mut is_login = false;

        loop {
            let mut buf = vec![0; 4096];
            let len = match r.read(&mut buf).await {
                Ok(len) => len,
                Err(e) => {
                    log::error!("抽水矿机 从服务器读取失败了。抽水 Socket 关闭 {:?}", e);
                    bail!("读取Socket 失败。可能矿池关闭了链接");
                }
            };

            if len == 0 {
                log::error!("抽水矿机 服务端断开连接 读取Socket 失败。收到0个字节");
                bail!("读取Socket 失败。收到0个字节");
            }

            let buffer = buf[0..len].split(|c| *c == b'\n');
            for buf in buffer {
                if buf.is_empty() {
                    continue;
                }

                #[cfg(debug_assertions)]
                debug!(
                    "❗ ------矿池到矿机捕获封包:{:?}",
                    String::from_utf8(buf.clone().to_vec()).unwrap()
                );
                if let Ok(rpc) = serde_json::from_slice::<ServerId1>(&buf) {
                    #[cfg(debug_assertions)]
                    debug!("收到抽水矿机返回 {:?}", rpc);
                    if rpc.id == CLIENT_LOGIN {
                        if rpc.result == true {
                            //info!("✅✅ 登录成功");
                            is_login = true;
                        } else {
                            log::error!(
                                "矿池登录失败，请尝试重启程序 {}",
                                String::from_utf8(buf.clone().to_vec()).unwrap()
                            );
                            bail!(
                                "❗❎ 矿池登录失败，请尝试重启程序 {}",
                                String::from_utf8(buf.clone().to_vec()).unwrap()
                            );
                        }
                        // 登录。
                    } else if rpc.id == CLIENT_SUBHASHRATE {
                        #[cfg(debug_assertions)]
                        info!("🚜🚜 算力提交成功");
                    } else if rpc.result && rpc.id == 0 {
                        #[cfg(debug_assertions)]
                        info!("👍👍 Share Accept");
                    } else {
                        #[cfg(debug_assertions)]
                        info!("❗❗ Share Reject");

                        crate::protocol::rpc::eth::handle_error(self.id, &buf);
                    }
                } else if let Ok(rpc) = serde_json::from_slice::<ServerId>(&buf) {
                    #[cfg(debug_assertions)]
                    debug!("收到抽水矿机返回 {:?}", rpc);
                    if rpc.id == CLIENT_LOGIN {
                        if rpc.result == true {
                            //info!("✅✅ 登录成功");
                            is_login = true;
                        } else {
                            log::error!(
                                "矿池登录失败，请尝试重启程序 {}",
                                String::from_utf8(buf.clone().to_vec()).unwrap()
                            );
                            bail!(
                                "❗❎ 矿池登录失败，请尝试重启程序 {}",
                                String::from_utf8(buf.clone().to_vec()).unwrap()
                            );
                        }
                        // 登录。
                    } else if rpc.id == CLIENT_SUBHASHRATE {
                        #[cfg(debug_assertions)]
                        info!("🚜🚜 算力提交成功");
                    } else if rpc.result && rpc.id == 0 {
                        #[cfg(debug_assertions)]
                        info!("👍👍 Share Accept");
                    } else {
                        #[cfg(debug_assertions)]
                        info!("❗❗ Share Reject");

                        crate::protocol::rpc::eth::handle_error(self.id, &buf);
                    }
                } else if let Ok(server_json_rpc) =
                    serde_json::from_slice::<ServerJobsWithHeight>(&buf)
                {
                    #[cfg(debug_assertions)]
                    debug!("Got jobs {:?}", server_json_rpc);
                    //新增一个share
                    if let Some(job_id) = server_json_rpc.result.get(0) {
                        #[cfg(debug_assertions)]
                        debug!("发送到等待队列进行工作: {}", job_id);
                        // 判断以submitwork时jobs_id 是不是等于我们保存的任务。如果等于就发送回来给抽水矿机。让抽水矿机提交。
                        let job = serde_json::to_string(&server_json_rpc)?;
                        mine_jobs_queue.try_send(Job::new(
                            self.id as u32,
                            job,
                            server_json_rpc.get_diff(),
                        ));
                    }
                } else if let Ok(server_json_rpc) = serde_json::from_slice::<ServerSideJob>(&buf) {
                    #[cfg(debug_assertions)]
                    debug!("Got jobs {:?}", server_json_rpc);
                    //新增一个share
                    if let Some(job_id) = server_json_rpc.result.get(0) {
                        #[cfg(debug_assertions)]
                        debug!("发送到等待队列进行工作: {}", job_id);
                        // 判断以submitwork时jobs_id 是不是等于我们保存的任务。如果等于就发送回来给抽水矿机。让抽水矿机提交。
                        let job = serde_json::to_string(&server_json_rpc)?;
                        mine_jobs_queue.try_send(Job::new(
                            self.id as u32,
                            job,
                            server_json_rpc.get_diff(),
                        ));
                    }
                } else if let Ok(server_json_rpc) = serde_json::from_slice::<Server>(&buf) {
                    #[cfg(debug_assertions)]
                    debug!("Got jobs {:?}", server_json_rpc);
                    //新增一个share
                    if let Some(job_id) = server_json_rpc.result.get(0) {
                        #[cfg(debug_assertions)]
                        debug!("发送到等待队列进行工作: {}", job_id);
                        // 判断以submitwork时jobs_id 是不是等于我们保存的任务。如果等于就发送回来给抽水矿机。让抽水矿机提交。
                        let job = serde_json::to_string(&server_json_rpc)?;
                        mine_jobs_queue.try_send(Job::new(
                            self.id as u32,
                            job,
                            server_json_rpc.get_diff(),
                        ));
                    }
                } else {
                    #[cfg(debug_assertions)]
                    debug!(
                        "❗ ------未捕获封包:{:?}",
                        String::from_utf8(buf.clone().to_vec()).unwrap()
                    );
                    log::error!(
                        "抽水矿机 ------未捕获封包:{}",
                        String::from_utf8(buf.clone().to_vec()).unwrap()
                    );
                    //TODO 上报
                }
            }
        }
    }

    async fn client_to_server<W>(
        &self,
        _: Arc<JobQueue>,
        job_send: broadcast::Sender<(u64, String)>,
        send: UnboundedSender<String>,
        mut w: WriteHalf<W>,
        recv: &mut UnboundedReceiver<String>,
    ) -> Result<()>
    where
        W: AsyncWriteExt,
    {
        let mut jobs_recv = job_send.subscribe();

        loop {
            tokio::select! {
                Some(client_msg) = recv.recv() => {

                    #[cfg(debug_assertions)]
                    debug!("-------- M to S RPC #{:?}", client_msg);
                    if let Ok(mut client_json_rpc) = serde_json::from_slice::<ClientWithWorkerName>(client_msg.as_bytes())
                    {
                        if client_json_rpc.method == "eth_submitWork" {
                            //client_json_rpc.id = 40;
                            client_json_rpc.id = 0; //TODO 以新旷工形式维护 这个旷工
                            client_json_rpc.worker = self.worker_name.clone();
                            //info!("✅✅ 抽水一个份额");
                        } else if client_json_rpc.method == "eth_submitHashrate" {
                            #[cfg(debug_assertions)]
                            if let Some(hashrate) = client_json_rpc.params.get(0) {
                                #[cfg(debug_assertions)]
                                debug!(
                                    "✅✅ 矿机 :{} 提交本地算力 {}",
                                    client_json_rpc.worker, hashrate
                                );
                            }
                        } else if client_json_rpc.method == "eth_submitLogin" {
                            #[cfg(debug_assertions)]
                            debug!("✅✅ 矿机 :{} 请求登录", client_json_rpc.worker);
                        } else {
                            #[cfg(debug_assertions)]
                            debug!("矿机传递未知RPC :{:?}", client_json_rpc);

                            log::error!("矿机传递未知RPC :{:?}", client_json_rpc);
                        }

                        let rpc = serde_json::to_vec(&client_json_rpc)?;
                        let mut byte = BytesMut::new();
                        byte.put_slice(&rpc[0..rpc.len()]);
                        byte.put_u8(b'\n');
                        let w_len = w.write_buf(&mut byte).await?;
                        if w_len == 0 {
                            bail!("矿池写入失败.0");
                        }
                    } else if let Ok(client_json_rpc) =
                        serde_json::from_slice::<Client>(client_msg.as_bytes())
                    {
                        let rpc = serde_json::to_vec(&client_json_rpc)?;
                        let mut byte = BytesMut::new();
                        byte.put_slice(&rpc[0..rpc.len()]);
                        byte.put_u8(b'\n');
                        let w_len = w.write_buf(&mut byte).await?;
                        if w_len == 0 {
                            bail!("矿池写入失败.1");
                        }
                    }
                }

                Ok((id,job)) = jobs_recv.recv() => {
                    if id == self.id {
                        send.send(job).unwrap();
                    }
                }
            }
        }
    }

    async fn login_and_getwork(
        &self,
        _: Arc<JobQueue>,
        _: broadcast::Sender<(u64, String)>,
        send: UnboundedSender<String>,
    ) -> Result<()> {
        // let worker_name = self.hostname.clone() + "_" + self.id.to_string().as_str();
        // let worker_name = worker_name.as_str();

        let login = ClientWithWorkerName {
            id: CLIENT_LOGIN,
            method: "eth_submitLogin".into(),
            params: vec![self.wallet.clone(), "x".into()],
            worker: self.hostname.clone(),
        };

        let login_msg = serde_json::to_string(&login)?;
        send.send(login_msg).unwrap();

        sleep(std::time::Duration::new(1, 0)).await;

        let eth_get_work = ClientGetWork {
            id: CLIENT_GETWORK,
            method: "eth_getWork".into(),
            params: vec![],
        };

        loop {
            //BUG 未计算速率。应该用速率除以当前总线程数。

            //计算速率
            let submit_hashrate = ClientWithWorkerName {
                id: CLIENT_SUBHASHRATE,
                method: "eth_submitHashrate".into(),
                params: [
                    format!("0x{:x}", calc_hash_rate(40000000, self.config.share_rate),),
                    hex::encode(self.hostname.clone()),
                ]
                .to_vec(),
                worker: self.hostname.clone(),
            };

            let submit_hashrate_msg = serde_json::to_string(&submit_hashrate)?;
            send.send(submit_hashrate_msg).unwrap();
            let eth_get_work_msg = serde_json::to_string(&eth_get_work)?;
            send.send(eth_get_work_msg).unwrap();

            sleep(std::time::Duration::new(5, 0)).await;
        }
    }
}
