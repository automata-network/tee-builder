use super::{BundlePool, PricePool, SeqPool};
use eth_types::{Signer};

pub struct TxPool {
    pub seq_pool: SeqPool,
    pub price_pool: PricePool,
    pub bundle_pool: BundlePool,
}

impl TxPool {
    pub fn new(signer: Signer, limit: usize) -> Self {
        Self {
            seq_pool: SeqPool::new(signer.clone(), limit),
            price_pool: PricePool::new(signer.clone(), limit),
            bundle_pool: BundlePool::new(),
        }
    }
}

