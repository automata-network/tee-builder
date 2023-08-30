use std::prelude::v1::*;

use base::format::debug;
use base::trace::Alive;
use block_builder::BlockStateFetcher;
use eth_client::{BeaconSlot, ExecutionClient};
use eth_types::{PoolTx, Signer};
use statedb::TrieState;
use std::sync::Arc;

#[derive(Default)]
pub struct App {
    alive: Alive,
}

impl apps::App for App {
    fn run(&self, env: apps::AppEnv) -> Result<(), String> {
        let args = super::Args::from_args(env.args);
        glog::info!("{:?}", args);
        let cfg = base::fs::read_file(&args.cfg).map_err(debug)?;
        let cfg: crate::Config = serde_json::from_slice(&cfg).map_err(debug)?;
        {
            let beacon_slot = BeaconSlot::new(12, 1606824023);
            let relay =
                mev_boost::Relay::new(self.alive.clone(), cfg.mev_boost_relay, beacon_slot.clone());
            let current_slot = beacon_slot.current() + 1;
            for i in 0..10 {
                let vd = match relay.get_validator_for_slot(current_slot) {
                    Ok(vd) => vd,
                    Err(err) => {
                        base::thread::sleep_ms(1000);
                        continue;
                    }
                };

                glog::info!("slot: {}, vd: {:?}", current_slot, vd);
                
                break;
            }
            // relay.get_validator_for_slot(next_slot)
        }
        return Ok(());
        // let signer = Signer::new(cfg.chain_id);

        // let client = self.el(&cfg)?;
        // let tx = client
        //     .get_transaction(&args.tx.as_str().into())
        //     .map_err(debug)?;
        // let mut block_number = args.block;
        // if block_number == 0 {
        //     block_number = tx.block_number.unwrap().as_u64() - 1;
        // }
        // glog::info!("blocknumber: {:?}", block_number);
        // let parent = client
        //     .get_block_header(block_number.into())
        //     .map_err(debug)?;
        // let parent = Arc::new(parent);
        // let store = statedb::TrieMemStore::new(cfg.trie_node_limit);
        // let simulator = block_builder::Simulator::new(
        //     cfg.chain_id,
        //     self.alive.clone(),
        //     cfg.simulator_thread,
        //     client.clone(),
        // );
        // let pooltx = PoolTx::with_tx(&signer, tx.inner().unwrap());
        // let fetcher = BlockStateFetcher::new(client.clone(), parent.number.into());
        // let state = TrieState::new(fetcher, parent.clone(), store);
        // let mut result = simulator
        //     .simulate(state, &parent, vec![pooltx].iter(), false, true)
        //     .map_err(debug)?;
        // let result = result.pop().unwrap();
        // for item in result.access_list.iter() {
        //     glog::info!("acl: {:?}", item)
        // }
        // glog::info!("{:?}", result);
        // Ok(())
    }

    fn terminate(&self) {
        glog::info!("try shutdown");
        self.alive.shutdown()
    }
}

impl App {
    fn el(&self, cfg: &crate::Config) -> Result<Arc<ExecutionClient>, String> {
        let mut mix_client = jsonrpc::MixRpcClient::new(None);
        mix_client
            .add_endpoint(&self.alive, &cfg.execution_nodes)
            .map_err(debug)?;
        Ok(Arc::new(ExecutionClient::new(mix_client)))
    }
}
