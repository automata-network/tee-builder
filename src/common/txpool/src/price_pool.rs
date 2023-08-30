use std::prelude::v1::*;

use super::Error;
use std::collections::btree_map::Entry;
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use eth_types::{PoolTx, Signer, TransactionInner, SH160, SH256, SU256};

pub const TXPOOL_USER_MAX_SIZE: usize = 20;

pub struct PricePool {
    pending: Mutex<BTreeMap<SH160, UserTxList>>,
    caches: Mutex<BTreeMap<SH256, Arc<TransactionInner>>>,
    limited_size: usize,
    signer: Signer,
}

impl PricePool {
    pub fn new(signer: Signer, limited_size: usize) -> Self {
        Self {
            signer,
            pending: Mutex::new(BTreeMap::new()),
            caches: Mutex::new(BTreeMap::new()),
            // prices: Mutex::new(BTreeMap::new()),
            limited_size,
        }
    }

    pub fn clear(&self) {
        self.pending.lock().unwrap().clear();
        self.caches.lock().unwrap().clear();
    }

    pub fn filter(&self, txs: Vec<TransactionInner>) -> Vec<TransactionInner> {
        txs.into_iter()
            .filter(|tx| !self.contains(&tx.hash()))
            .collect()
    }

    pub fn remove(&self, hash: &SH256) -> bool {
        let mut caches = self.caches.lock().unwrap();
        let mut pending = self.pending.lock().unwrap();

        let tx = {
            match caches.remove(hash) {
                Some(tx) => tx,
                None => return false,
            }
        };
        let caller = self.signer.sender(&tx);
        if let Some(list) = pending.get_mut(&caller) {
            if list.remove(tx.nonce()) {
                // glog::info!("remove hash: {:?}", hash);
                if list.len() == 0 {
                    pending.remove(&caller);
                }
                return true;
            }
        }
        return false;
    }

    pub fn contains(&self, hash: &SH256) -> bool {
        let caches = self.caches.lock().unwrap();
        caches.contains_key(hash)
    }

    pub fn list(
        &self,
        filter: Option<&mut BTreeMap<SH256, bool>>,
        base_fee: Option<SU256>,
        limit: usize,
    ) -> BTreeMap<SH160, Vec<Arc<PoolTx>>> {
        let mut empty = BTreeMap::new();
        let filter = filter.unwrap_or(&mut empty);
        let mut result = BTreeMap::new();
        let mut caches = self.caches.lock().unwrap();
        let mut pending = self.pending.lock().unwrap();
        let mut removed_addr = Vec::new();
        let mut total_len = 0;
        let mut scanned = 0;
        let mut accept = 0;

        for (addr, list) in pending.iter_mut() {
            total_len += list.len();
            scanned += 1;
            let (txs, dropped) = list.flatten(filter, base_fee.as_ref());
            if txs.len() > 0 {
                accept += 1;
                result.insert(addr.clone(), txs);
                if result.len() >= limit {
                    break;
                }
            }
            if dropped > 0 && total_len > self.limited_size {
                let removed =
                    list.clean_underprice(base_fee.as_ref(), total_len - self.limited_size);
                total_len -= removed.len();
                for hash in removed {
                    caches.remove(&hash);
                }
                if list.len() == 0 {
                    removed_addr.push(addr.clone());
                }
            }
        }
        for addr in removed_addr {
            pending.remove(&addr);
        }
        glog::info!("generate scanned:{}, accepted:{}", scanned, accept);
        result
    }

    pub fn push(&self, tx: PoolTx) -> Result<SH256, Error> {
        let hash = tx.hash;

        // glog::info!("add tx: {:?}", tx.hash());
        let caller = self.signer.sender(&tx.tx);

        let tx_info = Arc::new(tx);
        let mut caches = self.caches.lock().unwrap();
        let mut pending = self.pending.lock().unwrap();
        {
            match caches.entry(hash) {
                Entry::Occupied(_) => return Err(Error::AlreadyKnowned),
                Entry::Vacant(entry) => {
                    entry.insert(tx_info.tx.clone());
                }
            }
        }

        let _added = match pending.entry(caller) {
            Entry::Occupied(mut entry) => entry.get_mut().add(tx_info.clone(), 0),
            Entry::Vacant(entry) => {
                let mut list = UserTxList::new();
                let added = list.add(tx_info, 0);
                entry.insert(list);
                added
            }
        };
        return Ok(hash);
    }

    pub fn sender(&self, tx: &TransactionInner) -> SH160 {
        self.signer.sender(tx)
    }

    pub fn len(&self) -> usize {
        let caches = self.caches.lock().unwrap();
        caches.len()
    }

    pub fn account_len(&self) -> usize {
        let pending = self.pending.lock().unwrap();
        let mut total = 0;
        for item in pending.iter() {
            if item.1.len() > 0 {
                total += 1;
            }
        }
        total
    }
}

struct UserTxList {
    txs: SortedMap,
}

impl UserTxList {
    pub fn new() -> Self {
        Self {
            txs: SortedMap::new(),
        }
    }

    // pub fn contains(&self, tx: &TransactionInner) -> bool {
    //     self.txs.contains(tx)
    // }

    pub fn len(&self) -> usize {
        self.txs.len()
    }

    pub fn flatten(
        &self,
        filter: &mut BTreeMap<SH256, bool>,
        base_fee: Option<&SU256>,
    ) -> (Vec<Arc<PoolTx>>, usize) {
        self.txs.flatten(filter, base_fee)
    }

    pub fn clean_underprice(&mut self, base_fee: Option<&SU256>, limit: usize) -> Vec<SH256> {
        self.txs.clean_underprice(base_fee, limit)
    }

    pub fn add(&mut self, tx: Arc<PoolTx>, price_bump: u64) -> bool {
        let nonce = tx.tx.nonce();
        if let Some(old) = self.txs.get(nonce) {
            if old.tx.max_fee_per_gas().raw() >= tx.tx.max_fee_per_gas().raw()
                || old.tx.max_priority_fee_per_gas().raw() >= tx.tx.max_priority_fee_per_gas().raw()
            {
                return false;
            }

            if price_bump > 0 {
                let a_fee_cap = SU256::from(100 + price_bump) * old.tx.max_fee_per_gas();
                let a_tip = SU256::from(100 + price_bump) * old.tx.max_priority_fee_per_gas();
                let threshold_fee_cap = a_fee_cap / SU256::from(100u64);
                let threshold_tip = a_tip / SU256::from(100u64);
                if tx.tx.max_fee_per_gas() < &threshold_fee_cap
                    || tx.tx.max_priority_fee_per_gas() < &threshold_tip
                {
                    return false;
                }
            }
        }
        // self.costcap = tx.tx.cost(None).min(self.costcap);
        // self.gascap = tx.tx.gas().min(*self.gascap).into();

        self.txs.put(tx);
        return true;
    }

    pub fn remove(&mut self, nonce: u64) -> bool {
        if !self.txs.remove(nonce) {
            return false;
        }
        self.txs.filter(|old| nonce > old.tx.nonce());
        return true;
    }
}

struct SortedMap {
    items: BTreeMap<u64, Arc<PoolTx>>,
}

impl SortedMap {
    pub fn new() -> Self {
        Self {
            items: BTreeMap::new(),
        }
    }

    pub fn clean_underprice(&mut self, base_fee: Option<&SU256>, limit: usize) -> Vec<SH256> {
        let mut removed = Vec::new();
        for (nonce, tx) in self.items.iter().rev() {
            if removed.len() >= limit {
                break;
            }
            if tx.tx.effective_gas_tip(base_fee).is_none() {
                removed.push(*nonce);
            }
        }
        let mut removed_tx = Vec::with_capacity(removed.len());
        for item in &removed {
            if let Some(tx) = self.items.remove(item) {
                removed_tx.push(tx.hash);
            }
        }
        removed_tx
    }

    // pub fn contains(&self, tx: &TransactionInner) -> bool {
    //     self.items.contains_key(&tx.nonce())
    // }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn get(&self, nonce: u64) -> Option<&Arc<PoolTx>> {
        self.items.get(&nonce)
    }

    pub fn put(&mut self, info: Arc<PoolTx>) {
        let nonce = info.tx.nonce();
        self.items.insert(nonce, info);
        if self.items.len() > TXPOOL_USER_MAX_SIZE {
            self.items.pop_last();
        }
    }

    // pub fn remove_by_hash(&mut self, hash: SH256) -> bool {
    //     let mut nonce = None;
    //     for (n, tx) in &self.items {
    //         if tx.tx.hash() == hash {
    //             nonce = Some(*n);
    //             break;
    //         }
    //     }
    //     if let Some(nonce) = nonce {
    //         self.items.remove(&nonce);
    //         return true;
    //     }
    //     return false;
    // }

    pub fn remove(&mut self, nonce: u64) -> bool {
        if let Some(_) = self.items.remove(&nonce) {
            return true;
        }
        return false;
    }

    pub fn filter<F>(&mut self, filter: F)
    where
        F: Fn(&PoolTx) -> bool,
    {
        let mut removed = Vec::new();
        for (nonce, tx) in &self.items {
            if filter(tx) {
                removed.push(*nonce);
            }
        }
        for nonce in &removed {
            self.items.remove(nonce);
        }
    }

    // pub fn first(&self) -> Option<&Arc<PoolTx>> {
    //     self.items.first_key_value().map(|(k, v)| v)
    // }

    pub fn flatten(
        &self,
        filter: &mut BTreeMap<SH256, bool>,
        base_fee: Option<&SU256>,
    ) -> (Vec<Arc<PoolTx>>, usize) {
        let mut new_list = <Vec<Arc<PoolTx>>>::new();
        let mut not_check = false;
        let mut dropped = 0;
        for (nonce, tx_info) in &self.items {
            if !not_check {
                match filter.get(&tx_info.hash) {
                    Some(true) => continue,
                    Some(false) => break,
                    None => not_check = true, // we may receive the missing tx with lower nonce
                };
            }
            if tx_info.tx.effective_gas_tip(base_fee).is_none() {
                filter.insert(tx_info.hash, false);
                dropped += 1;
                break;
            }
            if let Some(last_tx) = new_list.last() {
                let old_nonce = last_tx.tx.nonce();
                if old_nonce >= *nonce || nonce - old_nonce != 1 {
                    break;
                }
            }
            if new_list.len() == 0 {
                new_list.reserve(self.items.len());
            }
            new_list.push(tx_info.clone());
        }
        (new_list, dropped)
    }
}
