use std::prelude::v1::*;

use std::sync::{mpsc, Arc};

use super::BuildEnv;
use base::trace::Alive;
use eth_client::ExecutionClient;
use eth_types::{BlockHeader, PoolTx, Signer, SH160, SU256};
use evm_executor::{Context, ExecuteError, Executor};
use statedb::StateDB;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use base::thread::PanicContext;

use threadpool::ThreadPool;

pub struct Simulator {
    env: Arc<BuildEnv>,
    client: Arc<ExecutionClient>,
    alive: Alive,
    tp: Mutex<ThreadPool>,
}

#[derive(Debug)]
pub struct SimulateResult<T> {
    pub tag: T,
    pub tx: PoolTx,
    pub err: Option<SimulateError>,
    pub du: Duration,
    // pub network: CounterResult,
}

#[derive(Debug)]
pub enum SimulateError {
    QueueStalled,
    Execute(ExecuteError),
    State(statedb::Error),
    UnexpectedExited,
    UnknownError,
}

impl From<ExecuteError> for SimulateError {
    fn from(e: ExecuteError) -> Self {
        Self::Execute(e)
    }
}

impl From<statedb::Error> for SimulateError {
    fn from(e: statedb::Error) -> Self {
        Self::State(e)
    }
}

impl Simulator {
    pub fn new(chain_id: SU256, alive: Alive, num: usize, client: Arc<ExecutionClient>) -> Self {
        let env = Arc::new(BuildEnv::new(chain_id));
        let tp = Mutex::new(threadpool::ThreadPool::new(num));
        Self {
            env,
            alive,
            tp,
            client,
        }
    }

    pub fn signer(&self) -> &Signer {
        &self.env.signer
    }

    fn simulate_inner<S>(
        env: &BuildEnv,
        mut state_db: S,
        header: Arc<BlockHeader>,
        tx: &PoolTx,
    ) -> Result<PoolTx, SimulateError>
    where
        S: StateDB,
    {
        let caller = env.signer.sender(&tx.tx);
        let ctx = Context {
            chain_id: &env.signer.chain_id,
            caller: &caller,
            cfg: &env.cfg,
            tx: &tx,
            precompile: &env.precompile_set,
            header: &header,
        };

        let result = Executor::dry_run(ctx, &mut state_db)?;
        let access_list = state_db.export_access_list(Some(&SH160::default()));
        state_db.flush()?;

        Ok(PoolTx {
            caller,
            tx: tx.tx.clone(),
            access_list: Arc::new(access_list),
            hash: tx.hash,
            gas: result.used_gas.as_u64(),
            allow_revert: true,
            block: state_db.parent().number.as_u64(),
            result: String::from_utf8_lossy(&result.err).into(),
        })
    }

    pub fn simulate<'a, I, S>(
        &self,
        state: S,
        header: &Arc<BlockHeader>,
        pooltxs: I,
        external: bool,
        force: bool,
    ) -> Result<Vec<PoolTx>, SimulateError>
    where
        S: StateDB,
        I: Iterator<Item = &'a PoolTx>,
    {
        let (len, _) = pooltxs.size_hint();
        let mut out = Vec::with_capacity(len);
        let receiver = {
            let (sender, receiver) = mpsc::channel();
            for (idx, pooltx) in pooltxs.enumerate() {
                out.push(None);
                self.simulate_async(
                    idx,
                    state.fork(),
                    header.clone(),
                    pooltx,
                    force,
                    external,
                    sender.clone(),
                )
            }
            receiver
        };

        for _ in 0..out.len() {
            match receiver.recv() {
                Ok(result) => match result.err {
                    Some(err) => return Err(err),
                    None => out[result.tag] = Some(result.tx),
                },
                Err(_) => return Err(SimulateError::UnexpectedExited),
            }
        }
        if out.iter().any(|n| n.is_none()) {
            return Err(SimulateError::UnknownError);
        }
        let out = out.into_iter().map(|e| e.unwrap()).collect();
        Ok(out)
    }

    pub fn simulate_async<T, S>(
        &self,
        tag: T,
        state_db: S,
        header: Arc<BlockHeader>,
        pooltx: &PoolTx,
        force: bool,
        external: bool,
        sender: mpsc::Sender<SimulateResult<T>>,
    ) where
        S: StateDB,
        T: Send + 'static,
    {
        let tp = self.tp.lock().unwrap();
        if tp.queued_count() >= tp.max_count() && !force {
            glog::info!(
                "thread pool stalled, {}/{}",
                tp.queued_count(),
                tp.max_count()
            );
            let _ = sender.send(SimulateResult {
                tag,
                tx: pooltx.clone(),
                err: Some(SimulateError::QueueStalled),
                du: Duration::from_secs(0),
                // network: CounterResult::new(),
            });
            return;
        }

        let task = {
            let alive = self.alive.clone();
            let client = self.client.clone();
            let env = self.env.clone();
            let start = Instant::now();
            let signer = self.signer().clone();
            let mut pooltx = pooltx.clone();
            move || {
                let _ctx = PanicContext(pooltx.hash);
                if !alive.is_alive() {
                    let _ = sender.send(SimulateResult {
                        tag,
                        tx: pooltx,
                        err: Some(SimulateError::QueueStalled),
                        du: start.elapsed(),
                        // network: CounterResult::new(),
                    });
                    return;
                }
                let parent = state_db.parent();
                if external
                    && pooltx
                        .tx
                        .effective_gas_tip(Some(&parent.base_fee_per_gas))
                        .is_some()
                {
                    let caller = signer.sender(&pooltx.tx);
                    let result =
                        client.create_access_list(&caller, &pooltx.tx, parent.number.into());
                    match result {
                        Ok(list) => {
                            // glog::info!("result: {:?}", list);
                            pooltx.access_list = Arc::new(list.access_list);
                            pooltx.gas = list.gas_used.as_u64();
                            pooltx.block = parent.number.as_u64();
                            pooltx.result = list.error.unwrap_or("".into());
                            let _ = sender.send(SimulateResult {
                                tag,
                                tx: pooltx,
                                err: None,
                                du: start.elapsed(),
                                // network: CounterResult::new(),
                            });
                            return;
                        }
                        Err(err) => {
                            // insufficient funds for gas * price + value?
                            glog::error!("simulate[{:?}] fail: [{:?}]", pooltx.hash, err);
                        }
                    }
                }
                let _guard = glog::set_tag("dry_run");
                let response = match Self::simulate_inner(&env, state_db, header, &pooltx) {
                    Ok(tx) => SimulateResult {
                        tag,
                        tx,
                        err: None,
                        du: start.elapsed(),
                        // network: fetcher.get_miss_usage(),
                    },
                    Err(err) => SimulateResult {
                        tag,
                        tx: pooltx,
                        err: Some(err),
                        du: start.elapsed(),
                        // network: fetcher.get_miss_usage(),
                    },
                };
                let _ = sender.send(response);
            }
        };
        tp.execute(task);
    }
}
