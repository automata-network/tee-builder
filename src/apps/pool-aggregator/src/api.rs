use std::{prelude::v1::*, sync::Mutex};

use apps::Getter;
use eth_client::{BlockReport, HashPool};
use eth_types::{HexBytes, PoolTx};
use jsonrpc::{JsonrpcErrorObj, RpcArgs, RpcServer, RpcServerApi, RpcServerSubscription};
use serde_json::BoxRawValue;
use std::collections::BTreeMap;
use std::ops::Deref;
use std::sync::Arc;

use crate::App;

#[derive(Clone)]
pub struct PoolAggregatorApi {
    hash_pool: Arc<HashPool>,

    block_reports: Arc<Mutex<BTreeMap<u64, BlockReport>>>,
    // subscriptions: Arc<Mutex<BTreeMap<String, ()>>>,
}

impl RpcServerApi<PoolTx> for PoolAggregatorApi {
    fn init_api(self: &Arc<Self>, srv: &mut RpcServer<Self, PoolTx>) {
        srv.jsonrpc("blocks", Self::blocks);
        srv.subscribe(self.clone());
    }
}

impl PoolAggregatorApi {
    fn blocks(&self, _args: RpcArgs) -> Result<BoxRawValue, JsonrpcErrorObj> {
        let block_reports = self.block_reports.lock().unwrap();
        Ok(serde_json::to_raw_value(block_reports.deref()).unwrap())
    }
}

impl RpcServerSubscription<PoolTx> for PoolAggregatorApi {
    fn methods(&self) -> (&'static str, &'static str, &'static str) {
        ("eth_subscribe", "eth_unsubscribe", "eth_subscription")
    }

    fn on_dispatch<'a>(&self, tx: &PoolTx, ids: Vec<&'a str>) -> Vec<(BoxRawValue, Vec<&'a str>)> {
        self.hash_pool.simulated(&tx.hash);
        let data = serde_json::to_raw_value(&tx.to_bytes()).unwrap();
        vec![(data, ids)]
    }

    fn on_subscribe(&self, _params: &str) -> Result<String, JsonrpcErrorObj> {
        let mut random = [0_u8; 16];
        crypto::read_rand(&mut random);
        let random = HexBytes::from(&random[..]);
        Ok(format!("{}", random))
    }

    fn on_unsubscribe(&self, _id: &str) -> bool {
        true
    }
}

impl Getter<PoolAggregatorApi> for App {
    fn generate(&self) -> PoolAggregatorApi {
        let hash_pool = self.hash_pool.get(self);
        let block_reports = self.block_reports.clone();
        PoolAggregatorApi {
            hash_pool,
            block_reports,
        }
    }
}
