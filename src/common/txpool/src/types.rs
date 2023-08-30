use std::prelude::v1::*;

use serde::{Deserialize, Serialize};

use eth_types::{Bundle, HexBytes, PoolTx, Signer, TransactionInner, SH160, SH256, SU256, SU64};
use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::sync::Arc;

#[derive(Debug)]
pub enum Error {
    AlreadyKnowned,
    ErrGasFeeCapTooLow,
}

pub struct TransactionsByPriceAndNonce {
    txs: BTreeMap<SH160, Vec<Arc<PoolTx>>>,
    heads: BinaryHeap<Reverse<TxWithMinerFee>>,
    signer: Signer,
    base_fee: SU256,
}

impl TransactionsByPriceAndNonce {
    pub fn new(
        signer: Signer,
        mut txs: BTreeMap<SH160, Vec<Arc<PoolTx>>>,
        base_fee: SU256,
    ) -> Self {
        let mut heads = BinaryHeap::new();
        // let mut total_remove = Duration::from_nanos(0);
        // let mut total_push = Duration::from_nanos(0);
        // let mut total_new = Duration::from_nanos(0);
        for (_, acc_txs) in &mut txs {
            if acc_txs.len() == 0 {
                continue;
            }
            // let acc = signer.sender(&acc_txs[0].tx);
            // let t0 = Instant::now();
            let wrapper = match TxWithMinerFee::new(acc_txs[0].clone(), Some(&base_fee)) {
                Ok(tx) => tx,
                Err(err) => {
                    glog::debug!(target: "txpool", "discard tx[{:?}]: {:?}", &acc_txs[0].tx.hash(), err);
                    continue;
                }
            };
            glog::debug!(target: "txpool", "head: {:?}", acc_txs.iter().map(|tx| tx.tx.hash()).collect::<Vec<_>>());
            // let t1 = Instant::now();
            acc_txs.remove(0);
            // let t2 = Instant::now();
            heads.push(Reverse(wrapper));
            // total_remove += t2 - t1;
            // total_push = t2.elapsed();
            // total_new = t1 - t0;
        }
        Self {
            txs,
            signer,
            base_fee,
            heads,
        }
    }

    pub fn head_len(&self) -> usize {
        self.heads.len()
    }

    pub fn len(&self) -> usize {
        let mut total = 0;
        for (_, tx) in &self.txs {
            total += tx.len();
        }
        total + self.heads.len()
    }

    pub fn pop(&mut self) {
        self.heads.pop();
    }

    pub fn shift(&mut self) {
        let info = match self.heads.peek() {
            Some(n) => n,
            None => return,
        };
        let acc = self.signer.sender(&info.0.tx.tx);
        if let Some(txs) = self.txs.get_mut(&acc) {
            if txs.len() > 0 {
                if let Ok(wrapper) = TxWithMinerFee::new(txs[0].clone(), Some(&self.base_fee)) {
                    self.heads.pop();
                    txs.remove(0);
                    self.heads.push(Reverse(wrapper));
                    return;
                }
            }
        }
        self.heads.pop();
    }

    pub fn peekn(&self, n: usize) -> Vec<Arc<PoolTx>> {
        let iter = self.heads.heads.iter();
        let mut list = Vec::with_capacity(n);
        for (item, _) in iter {
            list.push(item.0.tx.clone());
            if list.len() >= n {
                break;
            }
        }
        list
    }

    pub fn replace(&mut self, list: Vec<PoolTx>) {
        for tx in list {
            if let Ok(tx) = TxWithMinerFee::new(Arc::new(tx), Some(&self.base_fee)) {
                self.heads.heads.insert(Reverse(tx), ());
            }
        }
    }

    pub fn peek(&self) -> Option<&PoolTx> {
        self.heads.peek().map(|info| info.0.tx.as_ref())
    }
}

#[derive(Clone, Debug)]
pub struct TxWithMinerFee {
    tx: Arc<PoolTx>,
    miner_fee: SU256,
}

impl TxWithMinerFee {
    pub fn new(tx: Arc<PoolTx>, base_fee: Option<&SU256>) -> Result<Self, Error> {
        let miner_fee = match tx.tx.effective_gas_tip(base_fee) {
            Some(miner_fee) => miner_fee,
            None => return Err(Error::ErrGasFeeCapTooLow),
        };
        Ok(Self { tx, miner_fee })
    }
}

impl Ord for TxWithMinerFee {
    fn cmp(&self, other: &Self) -> core::cmp::Ordering {
        let result = self.miner_fee.cmp(&other.miner_fee);
        if result == core::cmp::Ordering::Equal {
            return self.tx.hash.cmp(&other.tx.hash);
        }
        result
    }
}

impl PartialOrd for TxWithMinerFee {
    fn partial_cmp(&self, other: &Self) -> Option<core::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Eq for TxWithMinerFee {}

impl PartialEq<Self> for TxWithMinerFee {
    fn eq(&self, other: &Self) -> bool {
        self.miner_fee.eq(&other.miner_fee) && self.tx.hash.eq(&other.tx.hash)
    }
}

#[derive(Clone, Debug)]
pub struct BinaryHeap<T: Ord + std::fmt::Debug> {
    heads: BTreeMap<T, ()>,
}

impl<T: Ord + std::fmt::Debug> BinaryHeap<T> {
    pub fn new() -> Self {
        Self {
            heads: BTreeMap::new(),
        }
    }

    pub fn push(&mut self, n: T) {
        self.heads.insert(n, ());
    }

    pub fn pop(&mut self) {
        self.heads.pop_first();
    }

    pub fn peek(&self) -> Option<&T> {
        self.heads.iter().next().map(|(tx, _)| tx)
    }

    pub fn len(&self) -> usize {
        self.heads.len()
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, Eq, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct SendBundleRequest {
    pub txs: Vec<HexBytes>,
    pub block_number: SU64,
    pub min_timestamp: Option<u64>,
    pub max_timestamp: Option<u64>,
    pub reverting_tx_hashes: Option<Vec<SH256>>,
    // String, UUID that can be used to cancel/replace this bundle
    pub uuid: Option<String>,
    // String, UUID that can be used to cancel/replace this bundle, has priority over `uuid` field
    pub replacement_uuid: Option<String>,
    // the percent(from 0 to 99) of full bundle ETH reward that should be passed back to the user(`refundRecipient`) at the end of the bundle
    pub refund_percent: Option<u64>,
    // Address, wallet that will receive the ETH reward refund from this bundle, default value - EOA of the first transaction inside the bundle
    pub refund_recipient: Option<SH160>,
}

impl SendBundleRequest {
    pub fn to_bundle(&self, signer: &Signer) -> Result<Bundle, String> {
        let reverting_tx_hashes = match &self.reverting_tx_hashes {
            Some(hashes) => hashes.clone(),
            None => Vec::new(),
        };
        let mut txs = Vec::with_capacity(self.txs.len());
        if self.txs.len() == 0 {
            return Err("empty transactions".into());
        }
        for tx in &self.txs {
            let tx = TransactionInner::from_bytes(&tx).map_err(|err| format!("{:?}", err))?;
            let hash = tx.hash();
            let mut allow_revert = false;
            if reverting_tx_hashes.contains(&hash) {
                allow_revert = true;
            }
            let mut tx = PoolTx::with_tx(signer, tx);
            tx.allow_revert = allow_revert;
            txs.push(tx);
        }
        let uuid = match &self.replacement_uuid {
            Some(uuid) => uuid.clone(),
            None => match &self.uuid {
                Some(uuid) => uuid.clone(),
                None => {
                    let mut n = vec![0_u8; 32];
                    crypto::read_rand(&mut n);
                    format!("{}", HexBytes::from(n))
                }
            },
        };
        Ok(Bundle {
            block_number: self.block_number,
            min_timestamp: self.min_timestamp,
            max_timestamp: self.max_timestamp,
            uuid,
            refund_percent: match self.refund_percent {
                None => 0,
                Some(v) => v,
            },
            refund_recipient: match self.refund_recipient {
                None => txs.first().unwrap().caller,
                Some(recipient) => recipient,
            },
            txs,
        })
    }
}
