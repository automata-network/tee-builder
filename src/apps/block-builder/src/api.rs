use std::prelude::v1::*;

use crate::{Args, BuildService};
use apps::Getter;
use base::time::now;
use base::trace::Alive;
use block_builder::{BlockStateFetcher, Simulator};
use eth_client::{BeaconSlot, ExecutionClient, HeadState};
use eth_types::{HexBytes, SH256, SU64};
use eth_types::{PoolTx, Signer, TransactionInner};
use jsonrpc::{JsonrpcClient, JsonrpcErrorObj, MixRpcClient, RpcArgs, RpcError, RpcServer};
use net_http::{HttpRequestReader, HttpResponse, HttpResponseBuilder};
use serde_json::BoxRawValue;
use statedb::TrieStore;
use statedb::{TrieMemStore, TrieState};
use std::fs::read_to_string;
use std::sync::{Arc, Mutex};
use txpool::{SendBundleRequest, TxPool};

use crate::App;

pub struct PublicApi {
    redirect: String,
    signer: Signer,
    methods: Vec<String>,
    el: Arc<ExecutionClient>,
    txpool: Arc<TxPool>,
    simulator: Arc<Simulator>,
    head_state: Arc<HeadState>,
    store: Arc<TrieMemStore>,
    build_service: Arc<BuildService>,
    beacon_slot: Arc<BeaconSlot>,
    args: Arc<Args>,
}

impl PublicApi {
    pub fn default(&self, args: RpcArgs<BoxRawValue>) -> Result<BoxRawValue, JsonrpcErrorObj> {
        if self.methods.iter().any(|item| item == args.method) {
            let response = match self.el.raw().rpc(args.method, &args.params) {
                Ok(n) => n,
                Err(RpcError::ResponseError(_, err)) => return Err(err),
                Err(err) => return Err(JsonrpcErrorObj::unknown(err)),
            };
            return Ok(response);
        }
        Err(JsonrpcErrorObj::client(format!(
            "method not found: {}",
            args.method
        )))
    }

    pub fn index(&self, _: HttpRequestReader) -> HttpResponse {
        HttpResponseBuilder::redirect(&self.redirect).build()
    }

    pub fn test(&self, _: HttpRequestReader) -> HttpResponse {
        let mut client = MixRpcClient::new(None);
        client
            .add_endpoint(&Alive::new(), &["ws://localhost:18233".to_owned()])
            .unwrap();
        
        #[cfg(feature = "sgx")]
        {
            let result = sgxlib_ra::exchange_key(self.args.enclave_id, client).unwrap();
            glog::info!("exchange result: {:?}", result);
        }
        HttpResponseBuilder::new(200).into()
    }

    pub fn send_raw_transaction(
        &self,
        args: RpcArgs<(HexBytes,)>,
    ) -> Result<SH256, JsonrpcErrorObj> {
        let tx = TransactionInner::from_bytes(&args.params.0)
            .map_err(|err| JsonrpcErrorObj::client(format!("invalid tx payload: {:?}", err)))?;
        let tx = PoolTx::with_tx(&self.signer, tx);
        let hash = tx.hash;
        let head = self.head_state.get();
        let fetcher = BlockStateFetcher::new(self.el.clone(), head.number.into());
        let state = TrieState::new(fetcher, head.clone(), self.store.fork());
        let result = self
            .simulator
            .simulate(state, &head, [tx].iter(), false, true);
        match result {
            Ok(result) => {
                for tx in result {
                    let _ = self.txpool.seq_pool.push(tx);
                }
            }
            Err(err) => return Err(JsonrpcErrorObj::client(format!("simulate fail: {:?}", err))),
        }
        Ok(hash)
    }

    pub fn chain_id(&self, _: RpcArgs) -> Result<SU64, JsonrpcErrorObj> {
        Ok(self.signer.chain_id.as_u64().into())
    }

    // pub fn send_bundle(
    //     &self,
    //     args: RpcArgs<(SendBundleRequest,)>,
    // ) -> Result<SH256, JsonrpcErrorObj> {
    //     let bundle = args
    //         .params
    //         .0
    //         .to_bundle(&self.signer)
    //         .map_err(|err| JsonrpcErrorObj::client(format!("parse bundle fail: {}", err)))?;
    //     let hash = bundle.hash();
    //     let head = self.head_state.get();
    //     let expired = head.number >= bundle.block_number
    //         || now()
    //             >= self.beacon_slot.next_block_time(
    //                 &head,
    //                 bundle
    //                     .block_number
    //                     .as_u64()
    //                     .saturating_sub(head.number.as_u64()),
    //             );

    //     glog::info!("received bundle: {:?}, expired: {}", bundle, expired);
    //     if expired {
    //         self.txpool
    //             .bundle_pool
    //             .set_history(&bundle, "too late".into());
    //         return Ok(hash);
    //     }

    //     let _ = self.txpool.bundle_pool.add(bundle);

    //     // so we interrupt the current build process, let it retry.
    //     self.build_service.rebuild();
    //     Ok(hash)
    // }

    pub fn get_bundle_list(&self, _: HttpRequestReader) -> HttpResponse {
        let result = self
            .txpool
            .bundle_pool
            .stat(|item| serde_json::to_vec(item))
            .unwrap();
        HttpResponseBuilder::new(200).json(result).into()
    }
}

impl Getter<PublicApi> for App {
    fn generate(&self) -> PublicApi {
        let cfg = self.cfg.get(self);
        let signer = self.signer.cloned(self);
        PublicApi {
            signer,
            head_state: self.head_state.get(self),
            simulator: self.simulator.get(self),
            redirect: cfg.server.redirect.clone(),
            el: self.el.get(self),
            methods: cfg.server.public_methods.clone(),
            txpool: self.txpool.get(self),
            store: self.store.get(self),
            build_service: self.build_service.get(self),
            beacon_slot: self.beacon_slot.get(self),
            args: self.args.get(self),
        }
    }
}

impl Getter<RpcServer<PublicApi>> for App {
    fn generate(&self) -> RpcServer<PublicApi> {
        let cfg = self.cfg.get(self);
        let port = self.args.get(self).port;
        let (tls_cert, tls_key) = match cfg.server.tls.as_str() {
            "" => (Vec::new(), Vec::new()),
            path => (
                read_to_string(format!("{}.crt", path)).unwrap().into(),
                read_to_string(format!("{}.key", path)).unwrap().into(),
            ),
        };
        let cfg = jsonrpc::RpcServerConfig {
            listen_addr: format!("0.0.0.0:{}", port),
            tls_cert,
            tls_key,
            http_max_body_length: Some(cfg.server.body_limit),
            ws_frame_size: 64 << 10,
            threads: cfg.server.workers,
        };
        let mut srv = RpcServer::new(self.alive.clone(), cfg, self.pub_api.get(self)).unwrap();
        srv.jsonrpc("eth_chainId", PublicApi::chain_id);
        // srv.jsonrpc("eth_sendBundle", PublicApi::send_bundle);
        srv.jsonrpc("eth_sendRawTransaction", PublicApi::send_raw_transaction);
        srv.http_get("/test", PublicApi::test);
        srv.http_get("/", PublicApi::index);
        srv.http_get("/bundles", PublicApi::get_bundle_list);
        srv.default_jsonrpc(PublicApi::default);
        srv
    }
}
