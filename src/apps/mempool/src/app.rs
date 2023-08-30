use core::time::Duration;
use std::{prelude::v1::*};

use apps::{Getter, Var, VarMutex, AppEnv};
use base::fs::parse_file;
use base::trace::Alive;
use block_builder::Simulator;
use eth_client::{BeaconSlot, ExecutionClient, HashPool, HeadState, TxFetcher};
use eth_types::{Signer, PoolItem};
use jsonrpc::{MixRpcClient, RpcServer, RpcServerConfig};
use statedb::TrieMemStore;
use txpool::{TxPool};

use crate::{Args, Config, MempoolApi};

#[derive(Default)]
pub struct App {
    pub alive: Alive,
    pub args: Var<Args>,
    pub cfg: Var<Config>,
    pub head_state: Var<HeadState>,
    pub signer: Var<Signer>,
    pub _tx_fetcher: Var<TxFetcher>,
    pub hash_pool: Var<HashPool>,
    pub api: Var<MempoolApi>,
    pub srv: VarMutex<RpcServer<MempoolApi, PoolItem>>,
    pub beacon_slot: Var<BeaconSlot>,
    pub el: Var<ExecutionClient>,
    pub txpool: Var<TxPool>,
    pub store: Var<TrieMemStore>,
    pub simulator: Var<Simulator>,
}

impl apps::App for App {
    fn run(&self, env: AppEnv) -> Result<(), String> {
        self.args.set(Args::from_args(env.args));

        let srv = self.srv.get(self);
        let mut srv = srv.lock().unwrap();

        base::thread::spawn("head-state".into(), {
            let head_state = self.head_state.get(self);
            let receiver = head_state.subscribe_new_block();
            let alive = self.alive.clone();
            let sec = Duration::from_secs(1);
            let txpool = self.txpool.get(self);
            move || {
                for blk in alive.recv_iter(&receiver, sec) {
                    txpool
                        .seq_pool
                        .remove_list(blk.header.number.as_u64(), &blk.transactions);
                }
            }
        });

        srv.run();
        Ok(())
    }

    fn terminate(&self) {
        self.alive.shutdown();
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
        HeadState::new(self.alive.clone(), &cfg.execution_node, cfg.block_time).unwrap()
    }
}

impl Getter<Signer> for App {
    fn generate(&self) -> Signer {
        Signer::new(self.cfg.get(self).chain_id)
    }
}

impl Getter<TxFetcher> for App {
    fn generate(&self) -> TxFetcher {
        let signer = self.signer.cloned(self);
        let tx_source = self.cfg.get(self).tx_source.clone();
        let hash_pool = self.hash_pool.cloned(self);
        let mut fetcher = TxFetcher::new(self.alive.clone(), signer, hash_pool);

        fetcher.start(tx_source).unwrap();
        fetcher
    }
}

impl Getter<HashPool> for App {
    fn generate(&self) -> HashPool {
        HashPool::new(self.cfg.get(self).tx_hashcache_size)
    }
}

impl Getter<RpcServer<MempoolApi, PoolItem>> for App {
    fn generate(&self) -> RpcServer<MempoolApi, PoolItem> {
        let context = self.api.get(self);
        let alive = self.alive.clone();
        let srv_cfg = &self.cfg.get(self).server;
        let mut cfg = RpcServerConfig {
            listen_addr: format!("0.0.0.0:{}", self.args.unwrap().port),
            threads: srv_cfg.workers,
            http_max_body_length: Some(2 << 20),
            ..Default::default()
        };
        cfg.tls(&srv_cfg.tls).unwrap();
        RpcServer::api(alive, cfg, context).unwrap()
    }
}

impl Getter<BeaconSlot> for App {
    fn generate(&self) -> BeaconSlot {
        let cfg = self.cfg.get(self);
        BeaconSlot::new(cfg.block_time, cfg.genesis_time)
    }
}

impl Getter<TxPool> for App {
    fn generate(&self) -> TxPool {
        TxPool::new(self.signer.cloned(self), 10)
    }
}

impl Getter<ExecutionClient> for App {
    fn generate(&self) -> ExecutionClient {
        let cfg = self.cfg.get(self);
        let mut client = MixRpcClient::new(None);
        client
            .add_endpoint(&self.alive, &[cfg.execution_node.clone()])
            .unwrap();
        ExecutionClient::new(client)
    }
}

impl Getter<TrieMemStore> for App {
    fn generate(&self) -> TrieMemStore {
        let cfg = self.cfg.get(self);
        TrieMemStore::new(cfg.trie_node_limit)
    }
}

impl Getter<Simulator> for App {
    fn generate(&self) -> Simulator {
        let cfg = self.cfg.get(self);
        let el = self.el.get(self);
        Simulator::new(cfg.chain_id, self.alive.clone(), cfg.simulator_thread, el)
    }
}
