use std::prelude::v1::*;

use super::Error;
use eth_types::{PoolTx, Signer, Transaction, TransactionInner, SH160, SH256, SU256};

use std::collections::BTreeMap;
use std::ops::DerefMut;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct SeqPool {
    signer: Signer,
    max: usize,
    list: Mutex<SeqPoolList>,
}

impl SeqPool {
    pub fn new(signer: Signer, max: usize) -> Self {
        Self {
            signer,
            max,
            list: Default::default(),
        }
    }

    pub fn len(&self) -> usize {
        let list = self.list.lock().unwrap();
        list.txs.len()
    }

    pub fn swap(&self, other: Arc<SeqPool>) {
        let mut old = self.list.lock().unwrap();
        let mut other = other.list.lock().unwrap();
        std::mem::swap(old.deref_mut(), other.deref_mut());
    }

    pub fn get(&self, hash: &SH256) -> Option<PoolTx> {
        let list = self.list.lock().unwrap();
        let n = list.txs.get(hash)?;
        Some(n.tx.clone())
    }

    pub fn filter(&self, txs: Vec<TransactionInner>) -> Vec<TransactionInner> {
        txs.into_iter()
            .filter(|tx| !self.contains(&tx.hash()))
            .collect()
    }

    pub fn mark_fail(&self, hash: &SH256, reason: String) {
        let mut list = self.list.lock().unwrap();
        if let Some(tx) = list.txs.get_mut(hash) {
            tx.failure_reason = Some(reason)
        }
    }

    pub fn get_reason(&self, hash: &SH256) -> Option<String> {
        let list = self.list.lock().unwrap();
        if let Some(tx) = list.txs.get(hash) {
            return tx.failure_reason.clone();
        }
        None
    }

    pub fn get_submit_time(&self, hash: &SH256) -> Option<Duration> {
        let list = self.list.lock().unwrap();
        if let Some(tx) = list.txs.get(hash) {
            return Some(tx.submit_time.elapsed());
        }
        None
    }

    pub fn get_submit_to(&self, hash: &SH256) -> Option<Vec<u64>> {
        let list = self.list.lock().unwrap();
        if let Some(tx) = list.txs.get(hash) {
            return Some(tx.submit_to.clone());
        }
        None
    }

    pub fn clear(&self) {
        let mut list = self.list.lock().unwrap();
        list.accounts.clear();
        list.txs.clear();
        list.order.clear();
    }

    pub fn contains(&self, hash: &SH256) -> bool {
        let list = self.list.lock().unwrap();
        list.txs.contains_key(hash)
    }

    pub fn push(&self, tx: PoolTx) -> Result<SH256, Error> {
        let mut list = self.list.lock().unwrap();
        if list.txs.contains_key(&tx.hash) {
            return Err(Error::AlreadyKnowned);
        }
        let hash = list.push(tx);
        while list.len() > self.max {
            if list.pop().is_none() {
                break;
            }
        }
        Ok(hash)
    }

    pub fn sender(&self, tx: &TransactionInner) -> SH160 {
        self.signer.sender(tx)
    }

    pub fn mark_submited(&self, number: u64, txs: &[SH256]) {
        let mut list = self.list.lock().unwrap();
        for hash in txs {
            if let Some(tx) = list.txs.get_mut(hash) {
                tx.failure_reason = None;
                if !tx.submit_to.contains(&number) {
                    tx.submit_to.push(number);
                    tx.submit_to.sort();
                }
            }
        }
    }

    pub fn get_live_time(&self, hash: &SH256) -> Option<Duration> {
        let list = self.list.lock().unwrap();
        match list.txs.get(hash) {
            Some(n) => Some(n.submit_time.elapsed()),
            None => None,
        }
    }

    pub fn list_by_seq(&self) -> Vec<PoolTx> {
        let list = self.list.lock().unwrap();
        let mut result = Vec::with_capacity(list.order.len());
        for (_, hash) in &list.order {
            if let Some(tx) = list.txs.get(hash) {
                result.push(tx.tx.clone());
            }
        }
        result
    }

    pub fn remove_tx_list(&self, txs: &[Transaction]) {
        let mut list = self.list.lock().unwrap();
        for tx in txs {
            list.remove(&tx.hash);
        }
    }

    pub fn remove_list(&self, number: u64, hashes: &[SH256]) {
        let mut list = self.list.lock().unwrap();
        for hash in hashes {
            list.remove(hash);
        }
        for (_, tx) in &mut list.txs {
            if let Some(idx) = tx.submit_to.iter().position(|n| *n == number) {
                tx.submit_to.remove(idx);
            }
        }
    }

    pub fn remove(&self, hash: &SH256) -> bool {
        let mut list = self.list.lock().unwrap();
        list.remove(hash)
    }
}

#[derive(Default)]
pub struct SeqPoolList {
    order: BTreeMap<SU256, SH256>,
    txs: BTreeMap<SH256, TxInfo>,
    accounts: BTreeMap<SH160, Vec<(u64, SH256)>>,
    seq: SU256,
}

impl SeqPoolList {
    pub fn seq(&mut self) -> SU256 {
        let val = self.seq.clone();
        self.seq = self.seq + SU256::from(1u64);
        val
    }

    pub fn pop(&mut self) -> Option<TxInfo> {
        let (_, hash) = self.order.pop_first()?;
        let tx = self.txs.remove(&hash)?;
        if let Some(list) = self.accounts.get_mut(&tx.tx.caller) {
            if let Some(idx) = list
                .iter()
                .position(|n| n.0 == tx.tx.tx.nonce() && n.1 == tx.tx.hash)
            {
                list.remove(idx);
            }
        }
        Some(tx)
    }

    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn push(&mut self, tx: PoolTx) -> SH256 {
        let hash = tx.hash;

        if !self.accounts.contains_key(&tx.caller) {
            self.accounts.insert(tx.caller.clone(), Vec::new());
        }
        let account = self.accounts.get_mut(&tx.caller).unwrap();
        let nonce = tx.tx.nonce();
        let mut reorder_idx = None;
        for (idx, (n, h)) in account.iter().enumerate() {
            if nonce == *n && &hash == h {
                return hash;
            }
            if nonce == *n {
                let tx = self.txs.remove(h); // remove the old tx
                if let Some(tx) = tx {
                    self.order.remove(&tx.seq);
                }
            }
            if nonce <= *n {
                reorder_idx = Some(idx);
                break;
            }
        }

        let seq = self.seq.clone();
        self.seq = self.seq + SU256::one();
        self.order.insert(seq, hash);

        if let Some(reorder_idx) = reorder_idx {
            let reorder_list = account.drain(reorder_idx..).collect::<Vec<_>>();
            account.push((nonce, hash));
            for (n, tx_hash) in reorder_list {
                if let Some(tx) = self.txs.get_mut(&tx_hash) {
                    self.order.remove(&tx.seq);
                    let seq = self.seq.clone();
                    self.seq = self.seq + SU256::one();
                    tx.seq = seq;
                    self.order.insert(tx.seq.clone(), tx_hash.clone());
                    account.push((n, tx_hash));
                }
            }
        } else {
            account.push((nonce, hash));
        }

        self.txs.insert(
            hash,
            TxInfo {
                tx,
                seq: seq.clone(),
                failure_reason: None,
                submit_time: Instant::now(),
                submit_to: Vec::new(),
            },
        );
        hash
    }

    pub fn remove(&mut self, hash: &SH256) -> bool {
        if let Some(tx) = self.txs.remove(hash) {
            if let Some(acc) = self.accounts.get_mut(&tx.tx.caller) {
                if let Some(idx) = acc.iter().position(|(_, h)| h == hash) {
                    acc.remove(idx);
                }
            }
            self.order.remove(&tx.seq);
            true
        } else {
            false
        }
    }
}

#[derive(Debug, Clone)]
pub struct TxInfo {
    pub tx: PoolTx,
    seq: SU256,
    // submited: bool,
    failure_reason: Option<String>,
    submit_time: Instant,
    submit_to: Vec<u64>,
}
