use std::prelude::v1::*;

use super::App;
use apps::Getter;
use base::time::{Time, Date};
use base::trace::Alive;
use block_builder::{BuildError, BuildPayload};
use eth_client::{BeaconHead, BlockReport};
use eth_client::{BeaconHeadState, BeaconSlot, ExecutionClient, HashPool, HeadState};
use statedb::TrieMemStore;
use statedb::TrieStore;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Duration;
use txpool::TxPool;

pub struct BuildService {
    alive: Alive,
    beacon_head_state: Arc<BeaconHeadState>,
    relay: Arc<mev_boost::Relay>,
    builder: Arc<block_builder::BlockBuilder>,
    beacon_slot: Arc<BeaconSlot>,
    store: Arc<TrieMemStore>,
    txpool: Arc<TxPool>,
    submit_time: Duration,
    current_alive: Mutex<Option<Alive>>,
}

impl BuildService {
    pub fn run(self: Arc<Self>) {
        for head in self.beacon_head_state.subscribe() {
            base::thread::spawn(head.thread_name(), {
                let srv = self.clone();
                move || srv.build_multiple_rounds(head)
            });
        }
    }

    pub fn rebuild(&self) {
        let current_alive = self.current_alive.lock().unwrap();
        current_alive.as_ref().map(|alive| alive.shutdown());
    }

    fn build_multiple_rounds(&self, mut head: BeaconHead) {
        let mut round = 0;
        while self.alive.is_alive() {
            if self.beacon_slot.current() != head.slot {
                break;
            }
            if !self.build_single_round(round, &mut head) {
                break;
            }
            round += 1;
        }
    }

    fn build_single_round(&self, round: usize, head: &mut BeaconHead) -> bool {
        let vd = match self.relay.get_validator_for_slot(head.slot + 1) {
            Ok(n) => n,
            Err(err) => {
                glog::error!("get validator fail: {:?}", err);
                return false;
            }
        };
        self.beacon_head_state.refresh(head);
        let new_slot = head.slot + 1;
        
        let deadline = self.beacon_slot.time(new_slot) - self.submit_time;
        if Time::now() >= deadline {
            return false;
        }
        if deadline.saturating_duration_since(Time::from_secs(head.block.timestamp.as_u64())) > Duration::from_secs(60) {
            glog::error!("block time lag > 1min, {:?}", Date::from(Time::from_secs(head.block.timestamp.as_u64())));
            return false;
        }
        let alive = self.alive.fork_with_deadline(deadline);
        *self.current_alive.lock().unwrap() = Some(alive.clone());

        glog::info!("slot:{}, vd: {:?}", head.slot + 1, vd);
        let payload = BuildPayload {
            round,
            slot: new_slot,
            base: head.block.clone(),
            coinbase: self.builder.cfg.payer,
            gas_limit: vd.gas_limit.into(),
            timestamp: self.beacon_slot.secs(new_slot),
            random: head.randao(),
            extra: self.builder.cfg.extra.clone().into(),
            withdrawals: head.withdrawal(),
            tips_recipient: Some(vd.fee_recipient),
        };
        match self
            .builder
            .build(&alive, self.store.fork(), &self.txpool, &payload)
        {
            Ok(blk) => {
                let deadline = deadline + self.submit_time;
                let now = Time::now();
                let available_for_submit =
                    deadline > now && deadline - now <= Duration::from_secs(3);
                glog::info!(
                    "remain_time(deadline): {:?}, available_for_submit: {}",
                    deadline.duration_since(now),
                    available_for_submit
                );
                if available_for_submit {
                    self.relay
                        .submit_block(blk.slot, &vd, &blk.block, blk.profit);
                    for bundle in blk.bundles {
                        self.txpool
                            .bundle_pool
                            .set_history(&bundle.bundle, bundle.status);
                    }
                }
            }
            Err(err) => {
                glog::error!("build fail: {:?}", err);
                if matches!(err, BuildError::NoTx) {
                    base::thread::sleep_ms(100);
                }
            }
        };
        return true;
    }
}

impl Getter<BuildService> for App {
    fn generate(&self) -> BuildService {
        let cfg = self.cfg.get(self);
        BuildService {
            alive: self.alive.clone(),
            beacon_head_state: self.beacon_head_state.get(self),
            relay: self.mev_boost_relay.get(self),
            builder: self.builder.get(self),
            beacon_slot: self.beacon_slot.get(self),
            store: self.store.get(self),
            txpool: self.txpool.get(self),
            submit_time: Duration::from_millis(cfg.mev_boost_relay.submit_time_millis),
            current_alive: Mutex::new(None),
        }
    }
}

pub struct BlockReports(BTreeMap<u64, BlockReport>);

impl Getter<BlockReports> for App {
    fn generate(&self) -> BlockReports {
        BlockReports(BTreeMap::new())
    }
}

pub struct RemoteBlockAnalyzer {
    beacon_slot: Arc<BeaconSlot>,
    head_state: Arc<HeadState>,
    hash_pool: HashPool,
    el: Arc<ExecutionClient>,
    block_reports: Arc<Mutex<BlockReports>>,
}

impl Getter<RemoteBlockAnalyzer> for App {
    fn generate(&self) -> RemoteBlockAnalyzer {
        RemoteBlockAnalyzer {
            beacon_slot: self.beacon_slot.get(self),
            head_state: self.head_state.get(self),
            hash_pool: self.hash_pool.cloned(self),
            el: self.el.get(self),
            block_reports: self.block_reports.get(self),
        }
    }
}

impl RemoteBlockAnalyzer {
    pub fn run(&self) {
        let slot = self.beacon_slot.clone();
        for blk in self.head_state.subscribe_new_head() {
            let blk = match self.el.get_block(blk.number.into()) {
                Ok(blk) => blk,
                Err(err) => {
                    glog::error!("{:?}", err);
                    continue;
                }
            };
            let hashes = blk
                .transactions
                .iter()
                .map(|item| item.hash)
                .collect::<Vec<_>>();
            let receipts = match self.el.get_receipts(&hashes) {
                Ok(receipts) => receipts,
                Err(err) => {
                    glog::error!("fetch receipts fail: {:?}", err);
                    continue;
                }
            };

            let report = self.hash_pool.report(&slot, blk, Some(&receipts));
            let mut block_reports = self.block_reports.lock().unwrap();
            glog::info!("report: {}", report);
            block_reports.0.insert(report.number, report);
            while block_reports.0.len() > 20 {
                block_reports.0.pop_first();
            }
        }
    }
}
