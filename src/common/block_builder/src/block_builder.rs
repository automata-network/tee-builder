use std::prelude::v1::*;

use super::types::*;
use super::Simulator;

use base::format::ternary;
use base::format::{ether_sub, parse_ether, truncate_ether};
use base::time::Date;
use base::trace::{Alive, AvgCounterResult, Slowlog};
use eth_client::ExecutionClient;
use eth_types::{
    Block, BlockHeader, LegacyTx, PoolTx, Receipt, Signer, TransactionAccessTuple,
    TransactionInner, Withdrawal, SH256, SU256, SU64,
};
use evm_executor::{BlockStateFetcher, ExecuteError, Executor};
use statedb::{StateDB, StateFetcher, TrieMemStore, TrieState, TrieStore};
use txpool::{TransactionsByPriceAndNonce, TxPool};

use std::sync::Arc;
use std::time::Instant;

pub struct BlockBuilder {
    pub cfg: Config,
    env: BuildEnv,
    client: Arc<ExecutionClient>,
    _simulator: Arc<Simulator>,
}

#[derive(Debug, Default)]
pub struct CommitStat {
    pub removed: u64,
    pub total: u64,
    pub failed: u64,
    pub state_miss: AvgCounterResult,
}

impl BlockBuilder {
    pub fn new(cfg: Config, client: Arc<ExecutionClient>, simulator: Arc<Simulator>) -> Self {
        let env = BuildEnv::new(cfg.chain_id);
        Self {
            cfg,
            env,
            client,
            _simulator: simulator,
        }
    }

    pub fn build(
        &self,
        alive: &Alive,
        store: TrieMemStore,
        txpool: &TxPool,
        payload: &BuildPayload,
    ) -> Result<BlockResult, BuildError> {
        let now = Instant::now();
        let mut env = self.prepare_work(store, payload);

        glog::info!("{}", "==".repeat(80));
        glog::info!(
            "\tROUND: #{}, Block: {}, Slot: {}",
            payload.round,
            env.header.number,
            payload.slot,
        );
        glog::info!(
            "\tBlockTime: {}, Pool {{ bundle: {}, seq: {}, price: {} }}, RemainTime: {:?}, Deadline: {:?}",
            env.header.timestamp,
            txpool.bundle_pool.len(),
            txpool.seq_pool.len(),
            txpool.price_pool.len(),
            alive.remain_time(),
            alive.deadline().map(Date::from),
        );
        glog::info!("{}", "==".repeat(80));

        let fill_result = self.fill_transactions(alive, txpool, &mut env)?;
        let block = self.finalize_and_assemble(
            env.header,
            &mut env.state,
            env.txs,
            &env.receipts,
            payload.withdrawals.clone(),
        )?;
        env.fetcher.get_miss_usage(); // clean up the dbGet
        {
            let header = &block.header;
            let pct = header.gas_used * SU64::from(100u64) / header.gas_limit;
            let reward = ether_sub(&env.state.get_balance(&header.miner)?, &env.miner_balance);
            glog::info!(
                "[{}.#{}] gas_used: {}({}%), builder_balance: {}, profit: {:?}, remain_time: {:?}",
                header.number,
                env.round,
                header.gas_used,
                pct,
                reward,
                parse_ether(&fill_result.profit, 18),
                alive.remain_time(),
            );
        }
        glog::info!(
            "{} generate work using time: {:?}, remain: {:?} {}",
            "-=".repeat(20),
            now.elapsed(),
            alive.remain_time(),
            "-=".repeat(20)
        );
        Ok(BlockResult {
            block,
            slot: payload.slot,
            bundles: fill_result.bundle_result,
            internal_txs: fill_result.internal_txs,
            profit: fill_result.profit,
            receipts: env.receipts,
        })
    }

    fn prepare_work(
        &self,
        store: TrieMemStore,
        payload: &BuildPayload,
    ) -> Environment<BlockStateFetcher, TrieState<BlockStateFetcher, TrieMemStore>> {
        let fetcher = BlockStateFetcher::new(self.client.clone(), payload.base.number.into());
        let state_db = TrieState::new(fetcher.clone(), payload.base.clone(), store.fork());
        let header = payload.next_block();

        Environment::new(
            fetcher,
            state_db,
            header,
            payload.tips_recipient,
            payload.round,
            store,
        )
    }

    fn fill_transactions<F: StateFetcher, D: StateDB>(
        &self,
        alive: &Alive,
        txpool: &TxPool,
        env: &mut Environment<F, D>,
    ) -> Result<FillResult, BuildError> {
        const TX_GAS: u64 = 32000;
        let mut result = FillResult::default();

        env.gas_pool = env.header.gas_limit.as_u64() - TX_GAS;

        self.prefetch_basic_info(env)?;

        env.miner_balance = env.state.get_balance(&env.header.miner)?;
        result.bundle_result = self.fill_bundles(alive, env, &txpool.bundle_pool)?;
        result.internal_txs = self.fill_seq(alive, env, &txpool.seq_pool)?;
        let price_stat = self.fill_price(alive, env, &txpool.price_pool)?;
        if env.txs.len() == 0 {
            return Err(BuildError::NoTx);
        }
        glog::info!("commit price_stat: {:?}", price_stat);
        result.profit = self.fill_tips(env, &result.internal_txs, result.bundle_result.len())?;
        Ok(result)
    }

    fn prefetch_basic_info<F, D>(&self, env: &mut Environment<F, D>) -> Result<(), BuildError>
    where
        F: StateFetcher,
        D: StateDB,
    {
        let now = Instant::now();
        let mut accounts = Vec::new();
        for addr in self.env.precompile_set.get_addresses() {
            accounts.push(TransactionAccessTuple::new(addr.into()));
        }
        accounts.push(TransactionAccessTuple::new(env.header.miner));
        if self.cfg.payer != env.header.miner {
            accounts.push(TransactionAccessTuple::new(self.cfg.payer));
        }
        if let Some(fee_recipient) = &env.tips_recipient {
            accounts.push(TransactionAccessTuple::new(*fee_recipient));
        }
        env.state.prefetch(accounts.iter())?;
        glog::info!("prefetch basic info: {:?} -> {:?}", now.elapsed(), accounts);
        Ok(())
    }

    fn fill_bundles<F, D>(
        &self,
        alive: &Alive,
        env: &mut Environment<F, D>,
        pool: &txpool::BundlePool,
    ) -> Result<Vec<BundleResult>, BuildError>
    where
        F: StateFetcher,
        D: StateDB,
    {
        let mut bundle_result = Vec::new();
        let block_number = env.header.number;
        let timestamp = env.header.timestamp.as_u64();
        let bundle_list = pool.list(block_number, timestamp);
        'next_bundle: for bundle in bundle_list {
            if !alive.is_alive() {
                break;
            }
            env.state
                .prefetch(bundle.txs.iter().map(|n| n.access_list.as_ref()).flatten())?;
            glog::info!(
                "[#{}] start bundle[{:?}]: tx:{}, state: {:?} ========",
                env.round,
                bundle.hash(),
                bundle.txs.len(),
                env.state.state_root(),
            );
            let state = env.state.state_root();
            let start_tcount = env.txs.len();
            for (idx, pool_tx) in bundle.txs.iter().enumerate() {
                let is_last = idx == bundle.txs.len() - 1;
                let hash = pool_tx.hash;
                let mut stop = false;
                let coinbase_before = env.state.get_balance(&env.header.miner)?;
                match self.commit_transaction(alive, env, pool_tx) {
                    Ok(CommitAction::Success(receipt)) => {
                        let is_succ = receipt.succ();
                        env.checked_txs.insert(hash, true);
                        if pool_tx.allow_revert || is_succ {
                            continue;
                        }
                        glog::error!("execute bundle fail[{:?}]: reverted", hash);
                    }
                    Ok(reason) => {
                        stop = matches!(reason, CommitAction::Stop(_));
                        glog::error!("execute bundle fail[{:?}]: {:?}", hash, reason);
                    }
                    Err(err) => return Err(BuildError::StateError(err)),
                }

                // so we revert this bundle
                env.state.revert(state);
                self.revert_txs_to(env, start_tcount);
                bundle_result.push(BundleResult {
                    bundle: bundle.clone(),
                    status: format!("reverted in tx: {:?}", pool_tx.hash),
                });
                if stop {
                    break 'next_bundle;
                }
                continue 'next_bundle;
            }
            let new_state = env.state.flush()?;
            env.fetcher.get_miss_usage();
            glog::info!(
                "[#{}] END bundle[{:?}] success, new state: {:?}",
                env.round,
                bundle.hash(),
                new_state,
            );
            bundle_result.push(BundleResult {
                bundle: bundle.clone(),
                status: "submitted".into(),
            })
        }
        Ok(bundle_result)
    }

    fn fill_seq<F, D>(
        &self,
        alive: &Alive,
        env: &mut Environment<F, D>,
        pool: &txpool::SeqPool,
    ) -> Result<Vec<u64>, BuildError>
    where
        F: StateFetcher,
        D: StateDB,
    {
        let mut internal_tx = Vec::new();
        let internal_list = pool.list_by_seq();
        env.state.prefetch(
            internal_list
                .iter()
                .map(|n| n.access_list.as_slice())
                .flatten(),
        )?;

        for pool_tx in internal_list {
            let hash = &pool_tx.hash;
            match self.commit_transaction(alive, env, &pool_tx) {
                Ok(CommitAction::Success(_)) => {
                    if let Some(du) = pool.get_live_time(hash) {
                        internal_tx.push(du.as_secs() / 12);
                    }
                }
                Ok(other) => {
                    glog::error!("[{}] commit fail: {:?}", hash, other);
                    match other {
                        CommitAction::MarkFail(reason) => {
                            pool.mark_fail(&hash, reason);
                        }
                        CommitAction::RemoveTx => {
                            pool.remove(&hash);
                        }
                        CommitAction::Stop(_) => {
                            break;
                        }
                        _ => {}
                    }
                }
                Err(err) => return Err(BuildError::StateError(err)),
            }
        }

        Ok(internal_tx)
    }

    fn fill_price<F, D>(
        &self,
        alive: &Alive,
        env: &mut Environment<F, D>,
        pool: &txpool::PricePool,
    ) -> Result<CommitStat, BuildError>
    where
        F: StateFetcher,
        D: StateDB,
    {
        let flow = PricePoolCommitFlow {
            signer: self.env.signer,
        };
        let stat = self.commit_pool(alive, env, pool, flow, 20)?;
        Ok(stat)
    }

    fn fill_tips<F, D>(
        &self,
        env: &mut Environment<F, D>,
        internals: &[u64],
        bundles: usize,
    ) -> Result<SU256, BuildError>
    where
        F: StateFetcher,
        D: StateDB,
    {
        let reward = self.calculate_profit(env)?;

        let recipient = match &env.tips_recipient {
            Some(recipient) => recipient,
            None => return Ok(0.into()),
        };

        let code = env.state.get_code(recipient)?;
        let tx_gas = match code.len() {
            0 => 21000,
            _other => 32000,
        };

        let tag = format!("{}#{}", env.header.number, env.round);
        let amount = self.cfg.tips_strategy.get(&tag, reward, internals, bundles);
        let mut tips_tx = {
            let fee = env.header.base_fee_per_gas * SU256::from(tx_gas);
            if amount < fee {
                return Err(BuildError::FeeTooLow);
            }
            let amount = amount - fee;
            TransactionInner::Legacy(LegacyTx {
                nonce: env.state.get_nonce(&self.cfg.payer)?.into(),
                gas_price: env.header.base_fee_per_gas,
                gas: tx_gas.into(),
                to: Some(recipient.clone()).into(),
                value: amount,
                ..Default::default()
            })
        };
        tips_tx.sign(&self.cfg.payer_sk, self.cfg.chain_id.as_u64());
        let pool_tx = PoolTx::with_tx(&self.env.signer, tips_tx);

        env.gas_pool += 32000;

        match self.commit_transaction(&Alive::new(), env, &pool_tx)? {
            CommitAction::Success(receipt) => {
                if !receipt.succ() {
                    return Err(BuildError::SendTipsFail("tx reverted".into()));
                }
            }
            other => return Err(BuildError::SendTipsFail(format!("{:?}", other))),
        }
        return Ok(pool_tx.tx.value());
    }

    fn revert_txs_to<F, D>(&self, env: &mut Environment<F, D>, at: usize)
    where
        F: StateFetcher,
        D: StateDB,
    {
        while env.txs.len() > at {
            let tx = match env.txs.pop() {
                Some(tx) => tx,
                None => break,
            };
            let receipt = env.receipts.pop().unwrap();
            env.checked_txs.remove(&tx.hash());
            env.refund_gas(receipt.gas_used.as_u64());
        }
    }

    fn calculate_profit<F, D>(&self, env: &mut Environment<F, D>) -> Result<SU256, BuildError>
    where
        F: StateFetcher,
        D: StateDB,
    {
        let current_miner_balance = env.state.get_balance(&env.header.miner)?;
        let balance_changed: SU256 = current_miner_balance
            .saturating_sub(*env.miner_balance)
            .into();
        let tx_tips = {
            let mut total_tips = SU256::zero();
            for (idx, tx) in env.txs.iter().enumerate() {
                let receipt = match env.receipts.get(idx) {
                    Some(receipt) => receipt,
                    None => return Err(BuildError::InternalError(format!("receipt not match"))),
                };
                let tips = match tx.reward(
                    receipt.gas_used.as_u64(),
                    Some(&env.header.base_fee_per_gas),
                ) {
                    Some(tips) => tips,
                    None => return Err(BuildError::InternalError(format!("internal error"))),
                };
                total_tips += tips;
            }
            total_tips
        };
        let profit = tx_tips.min(balance_changed);
        glog::info!(
            "calculated profit: balance_changed: {}, txfee: {}",
            parse_ether(&balance_changed, 18),
            parse_ether(&tx_tips, 18)
        );
        Ok(profit)
    }

    pub fn commit_transaction<'a, F, D>(
        &self,
        alive: &Alive,
        env: &'a mut Environment<F, D>,
        pool_tx: &PoolTx,
    ) -> Result<CommitAction<'a>, statedb::Error>
    where
        F: StateFetcher,
        D: StateDB,
    {
        // glog::info!("start tx: {:?}", pool_tx.hash);
        const TX_GAS: u64 = 21000;
        let tx = &pool_tx.tx;
        env.checked_txs.insert(tx.hash().into(), false);
        if env.gas_pool <= TX_GAS {
            return Ok(CommitAction::Stop(
                "Not enough gas for further transactions".into(),
            ));
        }
        if env.gas_pool < tx.gas_limit() {
            return Ok(CommitAction::Pop(format!(
                "gas pool out of limited, want:{}, remain: {}",
                tx.gas_limit(),
                env.gas_pool,
            )));
        }
        if !alive.is_alive() {
            return Ok(CommitAction::Stop(format!("not alive(maybe timeout)",)));
        }
        let effective_gas_tip = match tx.effective_gas_tip(Some(&env.header.base_fee_per_gas)) {
            Some(n) => n,
            None => {
                let err = ExecuteError::InsufficientBaseFee {
                    tx_hash: tx.hash(),
                    block_number: env.header.number.as_u64().into(),
                    block_base_fee_gwei: parse_ether(&env.header.base_fee_per_gas, 9),
                    base_fee_gwei: parse_ether(&tx.effective_gas_tip(None).unwrap(), 9),
                };
                return Ok(CommitAction::MarkFail(format!(
                    "invalid base fee: {:?}",
                    err
                )));
            }
        };

        let caller = tx.sender(&self.env.signer);
        let tx_hash = pool_tx.hash;

        // TODO: check whether tx protected
        let tx_idx = env.txs.len() as u64;
        let tx_start = Instant::now();
        let exec_ctx = evm_executor::Context {
            chain_id: &self.cfg.chain_id,
            caller: &caller,
            cfg: &self.env.cfg,
            tx: &pool_tx,
            precompile: &self.env.precompile_set,
            header: &env.header,
        };
        let receipt = match Executor::apply(exec_ctx, &mut env.state, tx_idx) {
            Ok(receipt) => {
                env.use_gas(receipt.gas_used.as_u64());
                env.txs.push(tx.clone());
                env.receipts.push(receipt);
                env.receipts.last().unwrap()
            }
            Err(err) => {
                match err {
                    ExecuteError::NonceTooLow | ExecuteError::NotSupported => {
                        env.checked_txs.insert(tx.hash().into(), true);
                        return Ok(CommitAction::RemoveTx);
                    }
                    ExecuteError::NonceTooHigh { .. } => {}
                    ExecuteError::ExecutePaymentTxFail(_) => {}
                    ExecuteError::InsufficientBaseFee { .. } => {}
                    ExecuteError::InsufficientFunds => {}
                    ExecuteError::StateError(err) => {
                        return Err(err);
                    }
                }

                return Ok(CommitAction::MarkFail(format!("{:?}", err)));
            }
        };

        let reward = ether_sub(
            &env.state.get_balance(&env.header.miner).unwrap(),
            &env.miner_balance,
        );

        let network = env.fetcher.get_miss_usage();
        if network.cnt > 0 {
            glog::info!(
                "[#{}][{}] access_list: {:?}",
                pool_tx.block,
                pool_tx.result,
                pool_tx.access_list
            );
        }

        glog::info!(
            "[#{}.{}][{}][{:?}][U{}][tip={}][rwd={}][elap={:?}][gas={}/{}][acl={}]{}",
            env.round,
            tx_idx,
            ternary(
                receipt.succ(),
                "SUCC",
                ternary(pool_tx.allow_revert, "REVERT", "FAIL")
            ),
            tx_hash,
            env.callers.index(&caller),
            truncate_ether(parse_ether(&effective_gas_tip, 9), 2),
            truncate_ether(reward, 6),
            tx_start.elapsed(),
            receipt.gas_used,
            env.gas_pool,
            pool_tx.access_list.len(),
            if network.cnt > 0 {
                format!("[net={}]", network)
            } else {
                String::new()
            },
        );

        env.checked_txs.insert(tx.hash().into(), true);
        return Ok(CommitAction::Success(receipt));
    }

    pub fn finalize_and_assemble<D>(
        &self,
        mut header: BlockHeader,
        state: &mut D,
        txs: Vec<Arc<TransactionInner>>,
        receipts: &[Receipt],
        withdrawals: Vec<Withdrawal>,
    ) -> Result<Block, BuildError>
    where
        D: StateDB,
    {
        let mut access_list = withdrawals
            .iter()
            .map(|item| TransactionAccessTuple::new(item.address))
            .collect::<Vec<_>>();
        access_list.sort_by(|a, b| a.address.cmp(&b.address));
        access_list.dedup();
        state.prefetch(access_list.iter())?;
        for withdrawal in &withdrawals {
            let amount = withdrawal.amount.as_u256() * eth_types::gwei();
            state.add_balance(&withdrawal.address, &amount.into())?;
        }

        header.state_root = state.flush()?;
        Ok(Block::new(header, txs, receipts, Some(withdrawals)))
    }

    fn commit_pool<P, F, D>(
        &self,
        alive: &Alive,
        env: &mut Environment<F, D>,
        pool: &P::Pool,
        flow: P,
        limit: usize,
    ) -> Result<CommitStat, BuildError>
    where
        P: CommitFlow,
        F: StateFetcher,
        D: StateDB,
    {
        let mut stat = CommitStat::default();
        let mut list = {
            let _trace = Slowlog::new_ms("generate txpool list", 10);
            flow.on_fetch_pool_list(env, pool)
        };

        'nextPage: while alive.is_alive() {
            let tx_list = flow.peekn(&list, limit);
            glog::info!(
                "peek tx: {}, checked: {}",
                tx_list.len(),
                env.checked_txs.len()
            );
            if tx_list.len() == 0 {
                break 'nextPage;
            }

            {
                // we get all the nonce
                // TODO: we don't need to get code

                let callers = tx_list
                    .iter()
                    .map(|n| TransactionAccessTuple::new(n.caller.clone()))
                    .collect::<Vec<_>>();
                env.state.prefetch(callers.iter())?;
            }

            {
                // prefetch, we should skip those txs which nonce is mismatch
                let valid_list = self.filter_nonce(env, &tx_list)?;

                // TODO: if the simulation is failed(reverted), we can simulate them again concurrently base on current state.

                let acls = valid_list
                    .iter()
                    .map(|item| item.access_list.iter())
                    .flatten();
                env.state.prefetch(acls)?;
            }

            loop {
                let pool_tx = match flow.peek(&list) {
                    Some(tx) => tx,
                    None => {
                        continue 'nextPage;
                    }
                };
                if !tx_list.iter().any(|item| item.hash == pool_tx.hash) {
                    // we got a new tx, try repeek
                    continue 'nextPage;
                }

                let commit_action = self.commit_transaction(alive, env, pool_tx);
                // glog::info!("{:?}act: {:?}", pool_tx.hash, commit_action);
                match commit_action {
                    Ok(CommitAction::Success(_)) => {
                        stat.total += 1;
                        // current_run += 1;
                        flow.shift(&mut list);
                    }
                    Ok(CommitAction::Shift) => flow.shift(&mut list),
                    Ok(CommitAction::Pop(_)) => flow.pop(&mut list),
                    Ok(CommitAction::MarkFail(_)) => {
                        stat.failed += 1;
                        flow.pop(&mut list);
                    }
                    Ok(CommitAction::RemoveTx) => {
                        stat.removed += 1;
                        flow.remove_tx(pool, &pool_tx.hash);
                        flow.shift(&mut list);
                    }
                    Ok(CommitAction::Stop(_)) => {
                        flow.pop(&mut list);
                        break 'nextPage;
                    }
                    Err(err) => {
                        glog::error!("{:?} {:?}", pool_tx.hash, err);
                        break 'nextPage;
                    }
                }
            }
        }

        env.state.flush()?;
        stat.state_miss = env.fetcher.get_miss_usage();
        Ok(stat)
    }

    fn filter_nonce<F, D>(
        &self,
        env: &mut Environment<F, D>,
        txs: &Vec<Arc<PoolTx>>,
    ) -> Result<Vec<Arc<PoolTx>>, BuildError>
    where
        F: StateFetcher,
        D: StateDB,
    {
        let mut out = Vec::with_capacity(txs.len());
        for tx in txs {
            let nonce = env.state.get_nonce(&tx.caller)?;
            if nonce != tx.tx.nonce() {
                if nonce > tx.tx.nonce() {
                    glog::debug!(target: "invalid-tx",
                        "tx[{:?}] out-of-date, nonce{} > tx.tx.nonce(){}",
                        tx.hash,
                        nonce,
                        tx.tx.nonce()
                    );
                }
                continue;
            }
            out.push(tx.clone());
        }
        Ok(out)
    }
}

pub trait CommitFlow {
    type Pool;
    type PoolOrderList;
    fn on_fetch_pool_list<F, D>(
        &self,
        env: &mut Environment<F, D>,
        pool: &Self::Pool,
    ) -> Self::PoolOrderList
    where
        F: StateFetcher,
        D: StateDB;
    fn peekn(&self, list: &Self::PoolOrderList, limit: usize) -> Vec<Arc<PoolTx>>;
    fn peek<'a>(&self, list: &'a Self::PoolOrderList) -> Option<&'a PoolTx>;
    fn shift(&self, list: &mut Self::PoolOrderList);
    fn pop(&self, list: &mut Self::PoolOrderList);
    fn remove_tx(&self, pool: &Self::Pool, hash: &SH256) -> bool;
}

pub struct PricePoolCommitFlow {
    signer: Signer,
}

impl CommitFlow for PricePoolCommitFlow {
    type Pool = txpool::PricePool;
    type PoolOrderList = TransactionsByPriceAndNonce;

    fn on_fetch_pool_list<F, D>(
        &self,
        env: &mut Environment<F, D>,
        pool: &Self::Pool,
    ) -> Self::PoolOrderList
    where
        F: StateFetcher,
        D: StateDB,
    {
        let list = pool.list(
            Some(&mut env.checked_txs),
            Some(env.header.base_fee_per_gas),
            usize::max_value(),
        );
        TransactionsByPriceAndNonce::new(self.signer.clone(), list, env.header.base_fee_per_gas)
    }

    fn peekn(&self, list: &Self::PoolOrderList, n: usize) -> Vec<Arc<PoolTx>> {
        list.peekn(n)
    }

    fn peek<'a>(&self, list: &'a Self::PoolOrderList) -> Option<&'a PoolTx> {
        list.peek()
    }

    fn pop(&self, list: &mut Self::PoolOrderList) {
        list.pop()
    }

    fn shift(&self, list: &mut Self::PoolOrderList) {
        list.shift()
    }

    fn remove_tx(&self, pool: &Self::Pool, hash: &SH256) -> bool {
        pool.remove(hash)
    }
}
