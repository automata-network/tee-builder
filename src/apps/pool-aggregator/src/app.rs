use std::prelude::v1::*;

use apps::{Getter, Var, VarMutex};
use base::fs::parse_file;
use base::trace::Alive;
use block_builder::{BlockStateFetcher, SimulateResult, Simulator};
use eth_client::{BeaconSlot, HashPool, MempoolItem, TxFetcher};
use eth_client::{BlockReport, ExecutionClient, HeadState};
use eth_types::{PoolTx, Signer};
use jsonrpc::{RpcServer, RpcServerConfig};
use statedb::MapState;
use statedb::StateDB;
use std::collections::BTreeMap;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::sync::Mutex;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::{Args, Config, PoolAggregatorApi};

#[derive(Default)]
pub struct App {
    pub alive: Alive,
    pub args: Var<Args>,
    pub cfg: Var<Config>,
    pub signer: Var<Signer>,
    pub head_state: Var<HeadState>,
    pub hash_pool: Var<HashPool>,
    pub el: Var<ExecutionClient>,
    pub block_reports: Arc<Mutex<BTreeMap<u64, BlockReport>>>,
    pub api: Var<PoolAggregatorApi>,
    pub simulator: Var<Simulator>,
    pub tx_fetcher: Var<TxFetcher>,
    pub srv: VarMutex<RpcServer<PoolAggregatorApi, PoolTx>>,
}

impl apps::App for App {
    fn run(&self, env: apps::AppEnv) -> Result<(), String> {
        self.args.set(Args::from_args(env.args));

        let (receiver, _handle) = self.simulate_thread()?;

        #[cfg(all(feature = "std", target_os = "linux"))]
        base::thread::spawn("mem-stat".into(), {
            let alive = self.alive.clone();
            move || loop {
                base::thread::sleep_ms(1000);
                let mem = procinfo::pid::statm_self().unwrap();
                glog::info!("mem: {:?}", mem);
                let page_size = 4096;
                if mem.resident * page_size > 1 << 30 {
                    alive.shutdown();
                    break;
                }
            }
        });

        base::thread::spawn("tx-analyzer".into(), {
            let el = self.el.get(self);
            let hash_pool = self.hash_pool.cloned(self);
            let block_reports = self.block_reports.clone();
            let cfg = self.cfg.get(self);
            let slot = BeaconSlot::new(cfg.block_time, cfg.genesis_time);
            let head_state = self.head_state.get(self);
            move || {
                for blk in head_state.subscribe_new_head() {
                    let blk = match el.get_block(blk.number.into()) {
                        Ok(blk) => blk,
                        Err(err) => {
                            glog::error!("{:?}", err);
                            continue;
                        }
                    };

                    let hashes = blk
                        .transactions
                        .iter()
                        .map(|tx| tx.hash)
                        .collect::<Vec<_>>();
                    let receipts = match el.get_receipts(&hashes) {
                        Ok(n) => n,
                        Err(err) => {
                            glog::error!("fetch receipt fail: {:?}", err);
                            continue;
                        }
                    };
                    let report = hash_pool.report(&slot, blk, Some(&receipts));
                    glog::info!("report: {}", report);
                    let mut block_reports = block_reports.lock().unwrap();
                    block_reports.insert(report.number, report);
                    while block_reports.len() > 20 {
                        block_reports.pop_first();
                    }
                }
            }
        });

        let srv = self.srv.get(self);
        let mut srv = srv.lock().unwrap();

        base::thread::spawn("subscription-forward".into(), {
            let alive = self.alive.clone();
            let sec = Duration::from_secs(1);
            let subscription_sender = srv.subscription_sender();
            move || {
                for tx in alive.recv_iter(&receiver, sec) {
                    let _ = subscription_sender.send(tx.tx);
                }
            }
        });

        srv.run();
        Ok(())
    }

    fn terminate(&self) {
        glog::info!("try shutdown");
        self.alive.shutdown()
    }
}

impl Getter<ExecutionClient> for App {
    fn generate(&self) -> ExecutionClient {
        let cfg = self.cfg.get(self);
        let mut mix_client = jsonrpc::MixRpcClient::new(None);
        mix_client
            .add_endpoint(&self.alive, &cfg.execution_nodes)
            .unwrap();
        ExecutionClient::new(mix_client)
    }
}

impl App {
    fn simulate_thread(&self) -> Result<(Receiver<SimulateResult<()>>, JoinHandle<()>), String> {
        let el = self.el.get(self);
        let (sender, receiver) = mpsc::channel();
        let secs = Duration::from_secs(1);
        let tx_fetcher = self.tx_fetcher.get(self);
        let head_state = self.head_state.get(self);
        let simulator = self.simulator.get(self);

        let handle = base::thread::spawn("tx-client".into(), move || {
            tx_fetcher.recv_iter(secs, |item| {
                let head_blk = head_state.get();
                let fetcher = BlockStateFetcher::new(el.clone(), head_blk.number.into());
                let state_db = MapState::new(head_blk.clone(), fetcher);

                let pooltx = match &item {
                    MempoolItem::Price(n) => n,
                    MempoolItem::Seq(n) => n,
                    MempoolItem::Bundle(_) => return,
                };
                simulator.simulate_async(
                    (),
                    state_db.fork(),
                    head_blk.clone(),
                    pooltx,
                    false,
                    true,
                    sender.clone(),
                );
            });
        });
        Ok((receiver, handle))
    }
}

impl Getter<Config> for App {
    fn generate(&self) -> Config {
        parse_file(&self.args.unwrap().cfg).unwrap()
    }
}

impl Getter<HeadState> for App {
    fn generate(&self) -> HeadState {
        let cfg = self.cfg.get(self);
        let ws_el_endpoint = cfg.get_el_endpoint("ws").unwrap();
        HeadState::new(self.alive.clone(), &ws_el_endpoint, cfg.block_time).unwrap()
    }
}

impl Getter<HashPool> for App {
    fn generate(&self) -> HashPool {
        let cfg = self.cfg.get(self);
        HashPool::new(cfg.tx_hashcache_size)
    }
}

impl Getter<Simulator> for App {
    fn generate(&self) -> Simulator {
        let cfg = self.cfg.get(self);
        let el = self.el.get(self);
        Simulator::new(cfg.chain_id, self.alive.clone(), cfg.simulator_thread, el)
    }
}

impl Getter<RpcServer<PoolAggregatorApi, PoolTx>> for App {
    fn generate(&self) -> RpcServer<PoolAggregatorApi, PoolTx> {
        let context = self.api.get(self);
        let alive = self.alive.clone();
        let cfg = RpcServerConfig {
            listen_addr: format!("0.0.0.0:{}", self.args.unwrap().port),
            threads: 10,
            http_max_body_length: Some(2 << 20),
            ..Default::default()
        };
        RpcServer::api(alive, cfg, context).unwrap()
    }
}

impl Getter<TxFetcher> for App {
    fn generate(&self) -> TxFetcher {
        let cfg = self.cfg.get(self);
        let signer = self.signer.cloned(self);
        let mut fetcher = TxFetcher::new(self.alive.clone(), signer, self.hash_pool.cloned(self));
        fetcher.start(cfg.tx_source.clone()).unwrap();
        fetcher
    }
}

impl Getter<Signer> for App {
    fn generate(&self) -> Signer {
        let chain_id = self.cfg.get(self).chain_id;
        Signer::new(chain_id)
    }
}
