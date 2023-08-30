use std::prelude::v1::*;

use std::collections::BTreeMap;
use std::sync::Arc;

use base::format::parse_ether;
use base::serde::deserialize_ether;
use base::trace::ItemIndexer;
use crypto::Secp256k1PrivateKey;
use eth_types::{
    Block, BlockHeader, Bundle, HexBytes, Receipt, Signer, TransactionInner, Withdrawal, SH160,
    SH256, SU256, SU64, U256,
};
use evm_executor::PrecompileSet;
use statedb::{StateDB, StateFetcher, TrieMemStore};

use serde::Deserialize;

#[derive(Deserialize, Debug, Clone)]
pub struct Config {
    pub chain_id: SU256,
    pub tips_strategy: TipsStrategy,

    #[serde(skip)]
    pub payer: SH160,
    pub payer_sk: Secp256k1PrivateKey,

    pub extra: String,
}

impl Config {
    pub fn init(&mut self) {
        self.payer = self.payer_sk.public().eth_accountid().into();
    }
}

pub struct BuildEnv {
    pub precompile_set: PrecompileSet,
    pub signer: Signer,
    pub cfg: evm_executor::Config,
}

impl BuildEnv {
    pub fn new(chain_id: SU256) -> Self {
        Self {
            precompile_set: evm_executor::PrecompileSet::berlin(),
            signer: Signer::new(chain_id),
            cfg: evm_executor::Config::shanghai(),
        }
    }
}

pub struct BuildResult {
    pub block: Block,
    pub bundles: Vec<BundleResult>,
    pub internal_txs: Vec<u64>,
    pub receipts: Vec<Receipt>,
    pub profit: SU256,
}

#[derive(Debug)]
pub struct BundleResult {
    pub bundle: Arc<Bundle>,
    pub status: String,
}

pub struct BuildPayload {
    pub round: usize,
    pub slot: u64,
    pub base: Arc<BlockHeader>,
    pub coinbase: SH160,
    pub gas_limit: SU64,
    pub timestamp: u64,
    pub random: SH256,
    pub extra: HexBytes,
    pub withdrawals: Vec<Withdrawal>,
    pub tips_recipient: Option<SH160>,
}

impl BuildPayload {
    pub fn next_block(&self) -> BlockHeader {
        let gas_limit =
            Self::calc_gas_limit(self.base.gas_limit.as_u64(), self.gas_limit.as_u64()).into();
        let base_fee = Self::calc_base_fee(
            self.base.gas_limit.as_u64(),
            self.base.gas_used.as_u64(),
            self.base.base_fee_per_gas.raw().clone(),
        );
        BlockHeader {
            parent_hash: self.base.hash(),
            number: self.base.number + SU64::from(1),
            gas_limit,
            timestamp: self.timestamp.into(),
            miner: self.coinbase,
            mix_hash: self.random,
            extra_data: self.extra.clone(),
            base_fee_per_gas: base_fee,
            difficulty: 0.into(),
            ..Default::default()
        }
    }

    pub fn calc_base_fee(gas_limit: u64, gas_used: u64, base_fee: U256) -> SU256 {
        const ELASTICITY_MULTIPLIER: u64 = 2;
        const BASE_FEE_CHANGE_DENOMINATOR: u64 = 8;
        let parent_gas_target = gas_limit / ELASTICITY_MULTIPLIER;
        if gas_used == parent_gas_target {
            return base_fee.into();
        }

        if gas_used > parent_gas_target {
            // If the parent block used more gas than its target, the baseFee should increase.
            // max(1, parentBaseFee * gasUsedDelta / parent_gas_target / BASE_FEE_CHANGE_DENOMINATOR)
            let mut num = U256::from(gas_used) - U256::from(parent_gas_target);
            num *= base_fee;
            num /= U256::from(parent_gas_target);
            num /= U256::from(BASE_FEE_CHANGE_DENOMINATOR);
            let base_fee_delta = num.max(1.into());

            return (base_fee_delta + base_fee).into();
        } else {
            // Otherwise if the parent block used less gas than its target, the baseFee should decrease.
            // max(0, parentBaseFee * gasUsedDelta / parent_gas_target / BASE_FEE_CHANGE_DENOMINATOR)
            let mut num = U256::from(parent_gas_target) - U256::from(gas_used);
            num *= base_fee;
            num /= U256::from(parent_gas_target);
            num /= U256::from(BASE_FEE_CHANGE_DENOMINATOR);
            let base_fee: U256 = base_fee - num;
            return base_fee.max(0.into()).into();
        }
    }

    pub fn calc_gas_limit(parent_gas_limit: u64, mut desired_limit: u64) -> u64 {
        const GAS_LIMIT_BOUND_DIVISOR: u64 = 1024;
        const MIN_GAS_LIMIT: u64 = 5000;
        let delta = parent_gas_limit / GAS_LIMIT_BOUND_DIVISOR - 1;
        let mut limit = parent_gas_limit;
        if desired_limit < MIN_GAS_LIMIT {
            desired_limit = MIN_GAS_LIMIT;
        }
        // If we're outside our allowed gas range, we try to hone towards them
        if limit < desired_limit {
            limit = parent_gas_limit + delta;
            if limit > desired_limit {
                limit = desired_limit;
            }
            return limit;
        }
        if limit > desired_limit {
            limit = parent_gas_limit - delta;
            if limit < desired_limit {
                limit = desired_limit;
            }
        }
        return limit;
    }
}

#[derive(Debug)]
pub enum BuildError {
    NoTx,
    StateError(statedb::Error),
    SendTipsFail(String),
    FeeTooLow,
    InternalError(String),
}

impl From<statedb::Error> for BuildError {
    fn from(err: statedb::Error) -> Self {
        Self::StateError(err)
    }
}

#[derive(Debug, Deserialize, Clone, Default, Eq, PartialEq)]
pub struct TipsStrategy {
    #[serde(deserialize_with = "deserialize_ether")]
    pub max: SU256,
    pub bundle: Option<TipsStrategyRule>,
    pub internal: Option<TipsStrategyRule>,
    pub normal: Option<TipsStrategyRule>,
}

impl TipsStrategy {
    pub fn get(&self, tag: &str, reward: SU256, internals: &[u64], bundles: usize) -> SU256 {
        let bundle = if let Some(bundle) = &self.bundle {
            if bundles > 0 {
                bundle.start
            } else {
                SU256::zero()
            }
        } else {
            SU256::zero()
        };

        let internal = if let Some(strategy) = &self.internal {
            let mut total = SU256::zero();
            if let Some(max_blk_miss) = internals.iter().max() {
                total += strategy.start;
                total += (SU256::from(internals.len() as u64) * strategy.num_increment)
                    .min(strategy.max_num_increment);
                total += (SU256::from(*max_blk_miss) * strategy.blocks_increment)
                    .min(strategy.max_blocks_increment);
            }
            total
        } else {
            SU256::zero()
        };

        let normal = if let Some(strategy) = &self.normal {
            strategy.start
        } else {
            SU256::zero()
        };

        let profit = reward + internal.max(bundle).max(normal).min(self.max);

        glog::info!(
            "[{}][profit={}] details: bundle({})={}, internal({:?})={}, normal={}, reward={}",
            tag,
            parse_ether(&profit, 18),
            bundles,
            parse_ether(&bundle, 18),
            internals,
            parse_ether(&internal, 18),
            parse_ether(&normal, 18),
            parse_ether(&reward, 18),
        );
        profit
    }
}

#[derive(Debug, Deserialize, Clone, Default, Eq, PartialEq)]
pub struct TipsStrategyRule {
    #[serde(deserialize_with = "deserialize_ether")]
    pub start: SU256,
    #[serde(deserialize_with = "deserialize_ether")]
    pub num_increment: SU256,
    #[serde(deserialize_with = "deserialize_ether")]
    pub max_num_increment: SU256,
    #[serde(deserialize_with = "deserialize_ether")]
    pub blocks_increment: SU256,
    #[serde(deserialize_with = "deserialize_ether")]
    pub max_blocks_increment: SU256,
}

pub struct Environment<F: StateFetcher, D: StateDB> {
    pub state: D,
    pub miner_balance: SU256,
    pub header: BlockHeader,
    pub txs: Vec<Arc<TransactionInner>>,
    pub receipts: Vec<Receipt>,
    pub gas_pool: u64,
    pub checked_txs: BTreeMap<SH256, bool>,
    pub tips_recipient: Option<SH160>,
    pub fetcher: F,
    pub round: usize,
    pub callers: ItemIndexer<SH160>,
    pub store: TrieMemStore,
}

impl<F: StateFetcher, D: StateDB> Environment<F, D> {
    pub fn new(
        fetcher: F,
        state: D,
        header: BlockHeader,
        tips_recipient: Option<SH160>,
        round: usize,
        store: TrieMemStore,
    ) -> Self {
        Self {
            state,
            fetcher,
            header,
            txs: Vec::new(),
            receipts: Vec::new(),
            gas_pool: Default::default(),
            miner_balance: SU256::zero(),
            checked_txs: BTreeMap::new(),
            tips_recipient,
            round,
            callers: ItemIndexer::new(),
            store,
        }
    }

    pub fn use_gas(&mut self, gas: u64) {
        self.gas_pool -= gas;
        self.header.gas_used += SU64::from(gas);
    }

    pub fn refund_gas(&mut self, gas: u64) {
        self.gas_pool += gas;
        self.header.gas_used -= SU64::from(gas);
    }
}

#[derive(Debug, Default)]
pub struct FillResult {
    pub bundle_result: Vec<BundleResult>,
    pub internal_txs: Vec<u64>,
    pub profit: SU256,
}

#[derive(Debug)]
pub enum CommitAction<'a> {
    Success(&'a Receipt),
    Shift,
    Pop(String),
    MarkFail(String),
    Stop(String),
    RemoveTx,
}

pub struct BlockResult {
    pub slot: u64,
    pub block: Block,
    pub bundles: Vec<BundleResult>,
    pub internal_txs: Vec<u64>,
    pub receipts: Vec<Receipt>,
    pub profit: SU256,
}
