use core::time::Duration;
use std::prelude::v1::*;

use super::Config;
use super::{Args, BuildService};
use crate::{BlockReports, PublicApi, RemoteBlockAnalyzer};
use apps::{var_cloned, var_get};
use apps::{AppEnv, Getter, Var};
use base::time::Time;
use base::trace::Alive;
use block_builder::Simulator;
use eth_client::{
    BeaconClient, BeaconHeadState, BeaconSlot, ExecutionClient, HashPool, HeadState, MempoolItem,
    TxFetcher,
};
use eth_types::Signer;
use jsonrpc::RpcServer;
use statedb::TrieMemStore;

use std::sync::Mutex;
use txpool::TxPool;

#[derive(Default)]
pub struct App {
    pub alive: Alive,

    pub args: Var<Args>,
    pub cfg: Var<Config>,
    pub signer: Var<Signer>,
    pub mev_boost_relay: Var<mev_boost::Relay>,
    pub pub_api: Var<PublicApi>,
    pub beacon_head_state: Var<BeaconHeadState>,
    pub builder: Var<block_builder::BlockBuilder>,
    pub cl: Var<BeaconClient>,
    pub el: Var<ExecutionClient>,
    pub beacon_slot: Var<BeaconSlot>,
    pub head_state: Var<HeadState>,
    pub hash_pool: Var<HashPool>,
    pub simulator: Var<Simulator>,
    pub store: Var<TrieMemStore>,
    pub txpool: Var<TxPool>,
    pub block_reports: Var<Mutex<BlockReports>>,
    pub tx_fetcher: Var<TxFetcher>,

    pub srv: Var<Mutex<RpcServer<PublicApi>>>,
    pub build_service: Var<BuildService>,
}

impl apps::App for App {
    fn run(&self, args: AppEnv) -> Result<(), String> {
        self.args.set(Args::from_args(args));
        self.cfg.get(self);

        glog::info!("{:?} {:?}", self.args, self.cfg);

        base::thread::spawn("collect-tx".into(), {
            let tx_fetcher = self.tx_fetcher.get(self);
            let _alive = self.alive.clone();
            let txpool = var_get!(self.txpool);
            let beacon_slot = var_cloned!(self.beacon_slot);
            let head_state = var_get!(self.head_state);
            let secs = Duration::from_secs(1);
            move || loop {
                tx_fetcher.recv_iter(secs, |item| match item {
                    MempoolItem::Bundle(bundle) => {
                        let head = head_state.get();
                        let next = beacon_slot.next_block_time(
                            &head,
                            bundle.block_number.as_u64() - head.number.as_u64(),
                        );
                        let dur = Time::from(next).duration_since(Time::now());
                        let _ = txpool.bundle_pool.add(bundle, &dur);
                    }
                    MempoolItem::Price(tx) => {
                        let _ = txpool.price_pool.push(tx);
                    }
                    MempoolItem::Seq(tx) => {
                        let _ = txpool.seq_pool.push(tx);
                    }
                });
            }
        });

        base::thread::spawn("tx-analyzer".into(), {
            let analyzer: RemoteBlockAnalyzer = self.generate();
            move || {
                analyzer.run();
            }
        });

        if !self.cfg.get(self).disable_build {
            base::thread::spawn("build".into(), {
                let build_service = self.build_service.get(self);
                move || {
                    build_service.run();
                }
            });
        }

        let _ = base::thread::spawn("jsonrpc-server".into(), {
            let srv = self.srv.get(self);
            move || {
                srv.lock().unwrap().run();
            }
        })
        .join();

        Ok(())
    }

    fn terminate(&self) {
        glog::info!("terminate block builder");
        self.alive.shutdown();
    }
}

impl App {
    pub fn compare_block(
        cl: &eth_client::ExecutionClient,
        block: &eth_types::Block,
        receipts: Option<&Vec<eth_types::Receipt>>,
    ) {
        use eth_client::BuildPayloadArgs;
        use eth_types::Block;
        let block = block.clone();
        let tx_list_bytes = block
            .transactions
            .iter()
            .map(|tx| eth_types::HexBytes::from(tx.clone().inner().unwrap().to_bytes()))
            .collect::<Vec<_>>();
        let args = BuildPayloadArgs {
            parent: block.header.parent_hash,
            timestamp: block.header.timestamp.as_u64(),
            feeRecipient: block.header.miner,
            random: block.header.mix_hash,
            withdrawals: block.withdrawals.clone(),
            txsBytes: tx_list_bytes,
        };

        let new_block = cl.seal_block(&args).unwrap();
        glog::info!("block diff: ================================================");
        if let Err(err) = Block::compare(&new_block, &block) {
            glog::info!("blk: {:?}", new_block.header);
            // glog::info!("expect blk: {:?} , submit tx: {}", expect_blk, test_txs_len);
            if let Some(receipts) = receipts {
                if block.header.receipts_root != new_block.header.receipts_root {
                    for receipt in receipts {
                        glog::info!("receipts: {:?}", receipt);
                    }
                }
            }
            println!("=============================================================================\n\n{}", err);
        }
        glog::info!("block diff end: ============================================");
    }
}
