use std::prelude::v1::*;

use crate::{App, PendingTransactionInfo, StatBundle};
use apps::Getter;
use base::time::Time;
use block_builder::Simulator;
use crypto::Secp256r1PublicKey;
use eth_client::{BeaconSlot, ExecutionClient, HeadState};
use eth_types::{
    BundleRlp, HexBytes, PoolItem, PoolTx, PoolTxRlp, Signer, TransactionInner, SH256,
};
use evm_executor::BlockStateFetcher;
use jsonrpc::{JsonrpcErrorObj, RpcArgs, RpcError, RpcServer, RpcServerApi, RpcServerSubscription};
use mempool::{GetBundleRequest, GetTxRequest, SubscribeOpt};
use net_http::{HttpRequestReader, HttpResponse, HttpResponseBuilder};
use serde_json::BoxRawValue;
use statedb::TrieStore;
use statedb::{TrieMemStore, TrieState};
use std::collections::BTreeMap;
use std::sync::mpsc;
use std::sync::Arc;
use std::sync::Mutex;
use txpool::{SendBundleRequest, TxPool};

pub struct MempoolApi {
    redirect: String,
    methods: Vec<String>,

    signer: Signer,
    head_state: Arc<HeadState>,
    txpool: Arc<TxPool>,
    beacon_slot: Arc<BeaconSlot>,
    el: Arc<ExecutionClient>,
    store: Arc<TrieMemStore>,
    simulator: Arc<Simulator>,

    subscribe_senders: Arc<Mutex<Option<mpsc::SyncSender<PoolItem>>>>,
    subscriptions: Arc<Mutex<BTreeMap<String, (Secp256r1PublicKey, SubscribeOpt)>>>,

    #[cfg(feature = "sgx")]
    ra_ctx: sgxlib_ra::RaServer,
}

#[cfg(feature = "sgx")]
sgxlib_ra::impl_jsonrpc_encrypt!(MempoolApi, ra_ctx);

impl RpcServerApi<PoolItem> for MempoolApi {
    fn init_api(self: &Arc<Self>, srv: &mut RpcServer<Self, PoolItem>) {
        srv.http_get("/", Self::index);
        srv.http_get("/pending", Self::tx_stat);
        srv.http_get("/bundles", Self::bundle_stat);

        srv.jsonrpc("eth_sendRawTransaction", Self::send_raw_transaction);
        srv.jsonrpc("eth_sendBundle", Self::send_bundle);

        #[cfg(feature = "sgx")]
        {
            srv.jsonrpc_sec("pool_getBundle", Self::get_bundle);
            srv.jsonrpc_sec("pool_getRawTransaction", Self::get_raw_transaction);
        }
        #[cfg(not(feature = "sgx"))]
        {
            srv.jsonrpc("pool_getBundle", Self::get_bundle);
            srv.jsonrpc("pool_getRawTransaction", Self::get_raw_transaction);
        }

        srv.default_jsonrpc(Self::default);
        srv.subscribe(self.clone());
        {
            let mut subscribe = self.subscribe_senders.lock().unwrap();
            if subscribe.is_some() {
                panic!("should not replace the subscribe");
            }
            *subscribe = Some(srv.subscription_sender());
        }

        #[cfg(feature = "sgx")]
        sgxlib_ra::Api::init_api(self.as_ref(), srv);
    }
}

#[cfg(feature = "sgx")]
impl sgxlib_ra::Api for MempoolApi {
    fn ctx(&self) -> &sgxlib_ra::RaServer {
        &self.ra_ctx
    }
}

impl MempoolApi {
    fn index(&self, _req: HttpRequestReader) -> HttpResponse {
        HttpResponseBuilder::redirect(&self.redirect).into()
    }

    fn tx_stat(&self, _req: HttpRequestReader) -> HttpResponse {
        let mut result = <Vec<PendingTransactionInfo>>::new();
        let pool = &self.txpool.seq_pool;
        for tx in pool.list_by_seq() {
            result.push(PendingTransactionInfo {
                hash: tx.hash,
                nonce: tx.tx.nonce(),
                caller: tx.caller,
                to: tx.tx.to(),
                value: tx.tx.value(),
                submit_time: format!("{:?} ago", pool.get_submit_time(&tx.hash).unwrap()),
            })
        }
        let body = serde_json::to_vec(&result).unwrap();
        HttpResponseBuilder::new(200).close().json(body).build()
    }

    fn bundle_stat(&self, _req: HttpRequestReader) -> HttpResponse {
        let list = self.txpool.bundle_pool.stat(|n| {
            n.iter()
                .map(|s| StatBundle {
                    hash: s.hash,
                    block_number: s.block_number,
                    status: s.status.clone(),
                    num_txs: s.num_txs,
                })
                .collect::<Vec<_>>()
        });
        let body = serde_json::to_vec(&list).unwrap();
        HttpResponseBuilder::new(200).close().json(body).build()
    }

    fn get_bundle(
        &self,
        args: RpcArgs<(GetBundleRequest,)>,
    ) -> Result<Vec<BundleRlp>, JsonrpcErrorObj> {
        let (req,) = args.params;

        let block_number = req.block_number;
        let block_timestamp = req
            .timestamp
            .map(|n| n.as_u64())
            .unwrap_or(base::time::now().as_secs());

        let list = self.txpool.bundle_pool.list(block_number, block_timestamp);
        let list = list.iter().map(|item| item.to_rlp()).collect::<Vec<_>>();
        Ok(list)
    }

    fn get_raw_transaction(
        &self,
        _: RpcArgs<(GetTxRequest,)>,
    ) -> Result<Vec<PoolTxRlp>, JsonrpcErrorObj> {
        let list = self.txpool.seq_pool.list_by_seq();
        let list: Vec<PoolTxRlp> = list.iter().map(|tx| tx.to_rlp()).collect();
        Ok(list)
    }

    fn send_raw_transaction(
        &self,
        args: RpcArgs<(TransactionInner,)>,
    ) -> Result<SH256, JsonrpcErrorObj> {
        let tx = PoolTx::with_tx(&self.signer, args.params.0);
        let hash = tx.hash;
        let head = self.head_state.get();

        let fetcher = BlockStateFetcher::new(self.el.clone(), head.number.into());
        let state = TrieState::new(fetcher, head.clone(), self.store.fork());
        let mut result = self
            .simulator
            .simulate(state, &head, [tx].iter(), false, true)
            .map_err(|err| JsonrpcErrorObj::client(format!("{:?}", err)))?;

        if let Some(tx) = result.pop() {
            if let Err(err) = self.txpool.seq_pool.push(tx.clone()) {
                return Err(JsonrpcErrorObj::client(format!("{:?}", err)));
            }

            self.subscribe_senders
                .lock()
                .unwrap()
                .as_ref()
                .map(|n| n.send(PoolItem::Tx(tx)));
        }

        Ok(hash)
    }

    fn send_bundle(&self, arg: RpcArgs<(SendBundleRequest,)>) -> Result<SH256, JsonrpcErrorObj> {
        let bundle = arg
            .params
            .0
            .to_bundle(&self.signer)
            .map_err(|err| JsonrpcErrorObj::client(format!("parse bundle fail: {}", err)))?;
        let hash = bundle.hash();
        let head = self.head_state.get();

        let next_block_time = Time::from(
            self.beacon_slot.next_block_time(
                &head,
                bundle
                    .block_number
                    .as_u64()
                    .saturating_sub(head.number.as_u64()),
            ),
        );
        let expired = head.number >= bundle.block_number || Time::now() >= next_block_time;
        glog::info!(
            "received bundle[{:?}]: {:?}, expired: {}",
            bundle.hash(),
            bundle,
            expired
        );
        let remain_time = next_block_time.duration_since(Time::now());
        if expired {
            self.txpool
                .bundle_pool
                .set_history(&bundle, format!("too late: {:?}", remain_time));
            return Ok(hash);
        }

        let _ = self.txpool.bundle_pool.add(bundle.clone(), &remain_time);
        self.subscribe_senders
            .lock()
            .unwrap()
            .as_ref()
            .map(|n| n.send(PoolItem::Bundle(bundle)));

        Ok(hash)
    }

    pub fn default(&self, args: RpcArgs<BoxRawValue>) -> Result<BoxRawValue, JsonrpcErrorObj> {
        if self.methods.iter().any(|item| item == args.method) {
            let response = match self.el.raw().rpc(args.method, &args.params) {
                Ok(n) => n,
                Err(RpcError::ResponseError(_, err)) => return Err(err),
                Err(err) => return Err(JsonrpcErrorObj::unknown(err)),
            };
            return Ok(response);
        }
        glog::warn!("method not found: {}", args.method);
        Err(JsonrpcErrorObj::client(format!(
            "method not found: {}",
            args.method
        )))
    }
}

impl Getter<MempoolApi> for App {
    fn generate(&self) -> MempoolApi {
        let cfg = &self.cfg.get(self).server;

        MempoolApi {
            methods: cfg.forward_methods.clone(),
            redirect: cfg.redirect.clone(),
            beacon_slot: self.beacon_slot.get(self),
            head_state: self.head_state.get(self),
            signer: self.signer.cloned(self),
            txpool: self.txpool.get(self),
            el: self.el.get(self),
            store: self.store.get(self),
            simulator: self.simulator.get(self),
            subscribe_senders: Arc::new(Mutex::new(None)),
            subscriptions: Default::default(),

            #[cfg(feature = "sgx")]
            ra_ctx: sgxlib_ra::RaServer::new(&cfg.ias_spid, &cfg.ias_apikey, true),
        }
    }
}

impl RpcServerSubscription<PoolItem> for MempoolApi {
    fn methods(&self) -> (&'static str, &'static str, &'static str) {
        ("pool_subscribe", "pool_unsubscribe", "pool_subscription")
    }

    #[cfg(feature = "sgx")]
    fn on_dispatch<'a>(
        &self,
        new_item: &PoolItem,
        ids: Vec<&'a str>,
    ) -> Vec<(BoxRawValue, Vec<&'a str>)> {
        use jsonrpc::RpcEncrypt;
        
        glog::info!("on dispatch: {:?} {:?}", new_item, ids);
        let subscriptions = self.subscriptions.lock().unwrap();

        let val = match new_item {
            PoolItem::Bundle(bundle) => serde_json::to_raw_value(&bundle.to_rlp()),
            PoolItem::Tx(tx) => serde_json::to_raw_value(&tx.to_rlp()),
        }
        .unwrap();

        let mut out = Vec::new();

        for id in ids {
            let (key, opt) = match subscriptions.get(id) {
                Some(n) => n,
                None => continue,
            };
            match (opt, new_item) {
                (SubscribeOpt::NewBundle, PoolItem::Bundle(_)) => {}
                (SubscribeOpt::NewTx, PoolItem::Tx(_)) => {}
                _ => continue,
            };

            let data = match self.encrypt(key, &val) {
                Ok(n) => n,
                Err(err) => {
                    glog::error!("encrypt fail: {:?}", err);
                    continue;
                }
            };
            let n = serde_json::to_raw_value(&data).unwrap();
            out.push((n, vec![id]));
        }
        out
    }

    #[cfg(not(feature = "sgx"))]
    fn on_dispatch<'a>(
        &self,
        new_item: &PoolItem,
        ids: Vec<&'a str>,
    ) -> Vec<(BoxRawValue, Vec<&'a str>)> {
        glog::info!("on dispatch: {:?} {:?}", new_item, ids);
        let new_ids = {
            let subscription = self.subscriptions.lock().unwrap();
            let mut new_ids = Vec::with_capacity(ids.len());
            for id in ids {
                match subscription.get(id).map(|(_pubkey, n)| (n, new_item)) {
                    Some((SubscribeOpt::NewBundle, PoolItem::Bundle(_))) => {}
                    Some((SubscribeOpt::NewTx, PoolItem::Tx(_))) => {}
                    _ => continue,
                }
                new_ids.push(id);
            }
            new_ids
        };

        if new_ids.len() > 0 {
            let data = match new_item {
                PoolItem::Bundle(bundle) => serde_json::to_raw_value(&bundle.to_rlp()),
                PoolItem::Tx(tx) => serde_json::to_raw_value(&tx.to_rlp()),
            }
            .unwrap();
            return vec![(data, new_ids)];
        }

        Vec::new()
    }

    fn on_subscribe(&self, params: &str) -> Result<String, JsonrpcErrorObj> {
        let args: (Secp256r1PublicKey, SubscribeOpt) =
            serde_json::from_str(params).map_err(|err| {
                JsonrpcErrorObj::client(format!("parse params fail: {:?} -> {}", err, params))
            })?;

        #[cfg(feature = "sgx")]
        {
            if self.ra_ctx.get_key(&args.0).is_none() {
                return Err(self.ra_ctx.unauth());
            }
        }

        let mut random = [0_u8; 16];
        crypto::read_rand(&mut random);
        let random = format!("{}", HexBytes::from(&random[..]));

        let mut subscriptions = self.subscriptions.lock().unwrap();
        subscriptions.insert(random.clone(), args);

        Ok(random)
    }

    fn on_unsubscribe(&self, id: &str) -> bool {
        let mut subscriptions = self.subscriptions.lock().unwrap();
        subscriptions.remove(id).is_some()
    }
}
