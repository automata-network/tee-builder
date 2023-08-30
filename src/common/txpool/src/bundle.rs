use std::prelude::v1::*;

use base::time::{now, SignedDuration};

use eth_types::{Bundle, SH256, SU64};

use serde::Serialize;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

pub struct BundlePool {
    list: Mutex<BundlePoolList>,
}

impl BundlePool {
    pub fn new() -> Self {
        Self {
            list: Mutex::new(BundlePoolList {
                history: Vec::new(),
                uuid: BTreeMap::new(),
            }),
        }
    }

    pub fn len(&self) -> usize {
        self.list.lock().unwrap().uuid.len()
    }

    pub fn add(&self, bundle: Bundle, dur: &SignedDuration) -> String {
        let bundle = Arc::new(bundle);
        let mut list = self.list.lock().unwrap();
        let old = list.uuid.insert(bundle.uuid.clone(), bundle.clone());
        if let Some(old) = old {
            let stat = list.get_stat(&old);
            if stat.status != "submitted" {
                stat.status = format!("replaced by {:?}", bundle.hash());
            }
        }
        let stat = list.get_stat(&bundle);
        stat.status = format!("pending: {:?}", dur);
        glog::info!(
            "add new bundle[{:?},blk:{}] {:?}",
            bundle.hash(),
            bundle.block_number,
            bundle
                .txs
                .iter()
                .map(|n| n.allow_revert)
                .collect::<Vec<_>>(),
        );
        bundle.uuid.clone()
    }

    pub fn get(&self, uuid: &str) -> Option<Arc<Bundle>> {
        let list = self.list.lock().unwrap();
        list.uuid.get(uuid).map(|n| n.clone())
    }

    pub fn set_history(&self, bundle: &Bundle, status: String) {
        let mut list = self.list.lock().unwrap();
        let stat = list.get_stat(bundle);
        stat.status = status;
        stat.process_time = now().as_secs();
    }

    pub fn stat<F, E>(&self, f: F) -> E
    where
        F: FnOnce(&Vec<BundleStat>) -> E,
    {
        let list = self.list.lock().unwrap();
        f(&list.history)
    }

    pub fn list(&self, block_number: SU64, block_timestamp: u64) -> Vec<Arc<Bundle>> {
        let mut out = Vec::new();
        let mut list = self.list.lock().unwrap();
        let mut removed = Vec::new();
        for (uuid, bundle) in &list.uuid {
            if block_number > bundle.block_number
                || block_timestamp > bundle.max_timestamp.unwrap_or(u64::max_value())
            {
                glog::info!(
                    "remove bundle[{:?},blk:{}] current: blk:{}",
                    bundle.hash(),
                    bundle.block_number,
                    block_number
                );
                removed.push(uuid.clone());
                continue;
            }
            if block_number < bundle.block_number
                || block_timestamp < bundle.min_timestamp.unwrap_or(0)
            {
                continue;
            }
            out.push(bundle.clone());
        }
        for uuid in &removed {
            if let Some(item) = list.uuid.remove(uuid) {
                list.get_stat(&item).status = "expired".into();
            }
        }
        out
    }
}

pub struct BundlePoolList {
    uuid: BTreeMap<String, Arc<Bundle>>,
    history: Vec<BundleStat>,
}

impl BundlePoolList {
    fn get_stat(&mut self, bundle: &Bundle) -> &mut BundleStat {
        let hash = bundle.hash();
        let idx = self.history.iter().position(|stat| stat.hash == hash);
        let idx = match idx {
            Some(idx) => idx,
            None => {
                self.history.insert(
                    0,
                    BundleStat {
                        hash,
                        block_number: bundle.block_number.as_u64(),
                        status: "unknown".into(),
                        num_txs: bundle.txs.len(),
                        process_time: 0,
                    },
                );
                self.history.truncate(100);
                0
            }
        };
        &mut self.history[idx]
    }
}

#[derive(Debug, Serialize, Clone, Default, Eq, PartialEq)]
pub struct BundleStat {
    pub hash: SH256,
    pub block_number: u64,
    pub status: String,
    pub num_txs: usize,
    pub process_time: u64,
}
