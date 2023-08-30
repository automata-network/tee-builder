use std::prelude::v1::*;

use super::{App, Args, Config};
use apps::Getter;
use block_builder::{BlockBuilder, Simulator};
use eth_client::{BeaconClient, BeaconHeadState, BeaconSlot, ExecutionClient, HashPool, HeadState, TxFetcher};
use eth_types::Signer;
use jsonrpc::MixRpcClient;
use statedb::TrieMemStore;
use txpool::TxPool;

impl Getter<Signer> for App {
    fn generate(&self) -> Signer {
        let cfg = self.cfg.get(self);
        Signer::new(cfg.builder.chain_id)
    }
}

impl Getter<mev_boost::Relay> for App {
    fn generate(&self) -> mev_boost::Relay {
        let beacon_slot = self.beacon_slot.cloned(self);
        mev_boost::Relay::new(
            self.alive.clone(),
            self.cfg.get(self).mev_boost_relay.clone(),
            beacon_slot,
        )
    }
}

impl Getter<Args> for App {
    fn generate(&self) -> Args {
        Args::default()
    }
}

impl Getter<Config> for App {
    fn generate(&self) -> Config {
        let data = std::fs::read_to_string(&self.args.get(self).cfg).unwrap();
        let mut cfg: Config = serde_json::from_str(&data).unwrap();
        cfg.init();
        cfg
    }
}

impl Getter<ExecutionClient> for App {
    fn generate(&self) -> ExecutionClient {
        let cfg = self.cfg.get(self);
        let mut client = MixRpcClient::new(None);
        client
            .add_endpoint(&self.alive, &cfg.execution_nodes)
            .unwrap();
        ExecutionClient::new(client)
    }
}

impl Getter<BeaconHeadState> for App {
    fn generate(&self) -> BeaconHeadState {
        BeaconHeadState::new(
            self.alive.clone(),
            self.head_state.cloned(self),
            self.beacon_slot.cloned(self),
            self.cl.get(self),
        )
    }
}

impl Getter<BeaconClient> for App {
    fn generate(&self) -> BeaconClient {
        let cfg = self.cfg.get(self);
        BeaconClient::new(cfg.beacon_endpoint.clone(), None)
    }
}

impl Getter<BeaconSlot> for App {
    fn generate(&self) -> BeaconSlot {
        let cl = self.cl.get(self);
        let cfg = self.cfg.get(self);
        let genesis = cl.genesis().unwrap();
        let genesis_time = genesis.data.genesis_time.as_u64();
        glog::info!("genesis_time: {}", genesis_time);
        BeaconSlot::new(cfg.block_time, genesis_time)
    }
}

impl Getter<HeadState> for App {
    fn generate(&self) -> HeadState {
        let cfg = self.cfg.get(self);
        let endpoint = cfg.get_el_endpoint("ws").unwrap();
        HeadState::new(self.alive.clone(), &endpoint, cfg.block_time).unwrap()
    }
}

impl Getter<BlockBuilder> for App {
    fn generate(&self) -> BlockBuilder {
        let cfg = self.cfg.get(self);
        let mut cfg = cfg.builder.clone();
        cfg.init();
        BlockBuilder::new(cfg, self.el.get(self), self.simulator.get(self))
    }
}

impl Getter<Simulator> for App {
    fn generate(&self) -> Simulator {
        let cfg = self.cfg.get(self);
        Simulator::new(
            cfg.builder.chain_id,
            self.alive.clone(),
            cfg.simulator_thread,
            self.el.get(self),
        )
    }
}

impl Getter<TxPool> for App {
    fn generate(&self) -> TxPool {
        let cfg = self.cfg.get(self);
        let signer = self.signer.cloned(self);
        TxPool::new(signer, cfg.txpool_size)
    }
}

impl Getter<HashPool> for App {
    fn generate(&self) -> HashPool {
        let cfg = self.cfg.get(self);
        HashPool::new(cfg.tx_hashcache_size)
    }
}

impl Getter<TxFetcher> for App {
    fn generate(&self) -> TxFetcher {
        let cfg = self.cfg.get(self);
        let signer = self.signer.cloned(self);
        let mut fetcher = TxFetcher::new(
            self.alive.clone(),
            signer,
            self.hash_pool.cloned(self),
        );
        mempool::ext::WithMempool::bind(&mut fetcher, self.args.get(self).enclave_id);
        fetcher.start(cfg.tx_source.clone()).unwrap();
        fetcher
    }
}

impl Getter<TrieMemStore> for App {
    fn generate(&self) -> TrieMemStore {
        TrieMemStore::new(self.cfg.get(self).trie_store_size)
    }
}
