use std::prelude::v1::*;

use base::format::debug;
use base::trace::{Alive, Slowlog};
use block_builder::SimulateResult;
use eth_client::{BlockReport, HashPool};
use eth_types::HexBytes;
use jsonrpc::{
    JsonrpcErrorObj, JsonrpcErrorResponse, JsonrpcRawRequest, JsonrpcRawResponse,
    JsonrpcResponseRawResult, ServerPushSubscription, ServerPushSubscriptionParams,
};
use net_http::{
    HttpRequestReader, HttpServerConns, HttpServerContext, HttpWsServer, HttpWsServerConfig,
    HttpWsServerContext, HttpWsServerHandler, TickResult, WsDataType, WsServerConns,
};
use std::collections::BTreeMap;
use std::ops::Deref;
use std::sync::mpsc::{self, TryRecvError};
use std::sync::{Arc, Mutex};

pub struct AggregatorServer {
    hash_pool: HashPool,
    block_reports: Arc<Mutex<BTreeMap<u64, BlockReport>>>,
    subscriber: mpsc::Receiver<SimulateResult<()>>,
    subscriptions: BTreeMap<String, usize>,
    responses: BTreeMap<usize, Vec<u8>>,
}

impl AggregatorServer {
    pub fn new(
        receiver: mpsc::Receiver<SimulateResult<()>>,
        block_reports: Arc<Mutex<BTreeMap<u64, BlockReport>>>,
        hash_pool: HashPool,
    ) -> Self {
        crate::AggregatorServer {
            hash_pool,
            block_reports,
            subscriber: receiver,
            subscriptions: BTreeMap::new(),
            responses: BTreeMap::new(),
        }
    }

    pub fn run(self, alive: Alive, listen: String) -> Result<(), String> {
        let cfg = HttpWsServerConfig {
            listen_addr: listen,
            tls_cert: vec![],
            tls_key: vec![],
            frame_size: 65 << 10,
            http_max_body_length: Some(2 << 20),
        };
        let mut svr = HttpWsServer::new(cfg, self).map_err(debug)?;
        while alive.is_alive() {
            match svr.tick() {
                TickResult::Busy => {}
                TickResult::Idle => {
                    base::thread::sleep_ms(10);
                }
                TickResult::Error => {
                    break;
                }
            }
        }
        Ok(())
    }
}

impl AggregatorServer {
    pub fn on_tick_response(
        &mut self,
        _: &mut HttpServerConns,
        ws_conns: &mut WsServerConns,
    ) -> TickResult {
        let mut removed = vec![];
        for (conn_id, data) in &self.responses {
            let n = match ws_conns.get_mut(*conn_id) {
                Some(n) => n,
                None => continue,
            };
            match n.write(data) {
                Ok(()) => {}
                Err(err) => {
                    glog::error!("write to remote[{}] fail: {:?}", conn_id, err);
                    continue;
                }
            }
            removed.push(*conn_id);
        }
        for id in &removed {
            self.responses.remove(&id);
        }
        if removed.len() > 0 {
            TickResult::Busy
        } else {
            TickResult::Idle
        }
    }

    pub fn on_tick_subscriptions(
        &mut self,
        _: &mut HttpServerConns,
        ws_conns: &mut WsServerConns,
    ) -> TickResult {
        match self.subscriber.try_recv() {
            Ok(tx) => {
                self.hash_pool.simulated(&tx.tx.hash);

                let data = tx.tx.to_bytes();
                let data = serde_json::to_raw_value(&data).unwrap();

                let mut push = ServerPushSubscription {
                    jsonrpc: "2.0".into(),
                    method: "eth_subscription".into(),
                    params: ServerPushSubscriptionParams {
                        subscription: "".into(),
                        result: data,
                    },
                };
                let mut removed_subscriptions = Vec::new();
                for (sid, cid) in &self.subscriptions {
                    let server = match ws_conns.get_mut(*cid) {
                        Some(n) => n,
                        None => continue,
                    };
                    push.params.subscription = sid.clone();
                    let data = serde_json::to_vec(&push).unwrap();
                    match server.write_ty(WsDataType::Text, data.as_slice()) {
                        Ok(()) => {}
                        Err(err) => {
                            glog::error!("{:?}", err);
                            ws_conns.remove(*cid);
                            removed_subscriptions.push(sid.clone());
                        }
                    }
                }
                for item in &removed_subscriptions {
                    self.subscriptions.remove(item);
                }
                TickResult::Busy
            }
            Err(TryRecvError::Disconnected) => TickResult::Error,
            Err(TryRecvError::Empty) => TickResult::Idle,
        }
    }

    fn write_error(&mut self, cid: usize, id: Option<jsonrpc::Id>, error: JsonrpcErrorObj) {
        let response = JsonrpcResponseRawResult::Err(JsonrpcErrorResponse {
            jsonrpc: "2.0".into(),
            id,
            error,
        });
        self.write_response(cid, response)
    }

    fn write_response(&mut self, cid: usize, data: JsonrpcResponseRawResult) {
        let data = match data {
            JsonrpcResponseRawResult::Ok(n) => serde_json::to_vec(&n),
            JsonrpcResponseRawResult::Err(n) => serde_json::to_vec(&n),
        }
        .unwrap();
        self.responses.insert(cid, data);
    }
}

impl HttpWsServerHandler for AggregatorServer {
    fn on_close_ws_conn(&mut self, _: usize) {
        let mut keys = vec![];
        for (key, _) in &self.subscriptions {
            keys.push(key.clone());
        }
        for key in &keys {
            glog::info!("remove subscription[conn closed]: {}", key);
            self.subscriptions.remove(key);
        }
    }

    fn on_new_http_request(&mut self, ctx: &mut HttpServerContext, _: HttpRequestReader) {
        // unimplemented!()
        ctx.is_close = true;
    }

    fn on_new_ws_conn(&mut self, _: &mut HttpWsServerContext) {
        // unimplemented!()
    }

    fn on_tick(
        &mut self,
        http_conns: &mut HttpServerConns,
        ws_conns: &mut WsServerConns,
    ) -> TickResult {
        let _trace = Slowlog::new_ms("SubscriptionServer.tick()", 100);
        let mut result = self.on_tick_response(http_conns, ws_conns);
        result |= self.on_tick_subscriptions(http_conns, ws_conns);
        result
    }

    fn on_new_ws_request(&mut self, ctx: &mut HttpWsServerContext, _: WsDataType, data: Vec<u8>) {
        glog::info!("on request: {}", String::from_utf8_lossy(&data));
        let req: JsonrpcRawRequest = match serde_json::from_slice(&data) {
            Ok(n) => n,
            Err(err) => {
                glog::info!("unexpected data: {:?}", String::from_utf8_lossy(&data));
                self.write_error(ctx.conn_id, None, JsonrpcErrorObj::unknown(err));
                return;
            }
        };
        match req.method.as_str() {
            "blocks" => {
                let result = {
                    let block_reports = self.block_reports.lock().unwrap();
                    serde_json::to_raw_value(block_reports.deref()).unwrap()
                };
                self.write_response(
                    ctx.conn_id,
                    JsonrpcResponseRawResult::Ok(JsonrpcRawResponse {
                        jsonrpc: "2.0".into(),
                        result,
                        id: req.id,
                    }),
                )
            }
            "eth_subscribe" => {
                let mut random = [0_u8; 16];
                crypto::read_rand(&mut random);
                let random = HexBytes::from(&random[..]);
                self.subscriptions
                    .insert(format!("{}", random), ctx.conn_id);

                glog::info!("add subscription: {}", random);
                let result = serde_json::to_raw_value(&random).unwrap();
                self.write_response(
                    ctx.conn_id,
                    JsonrpcResponseRawResult::Ok(JsonrpcRawResponse {
                        jsonrpc: "2.0".into(),
                        result,
                        id: req.id,
                    }),
                )
            }
            "eth_unsubscribe" => {
                let subscription_id: Vec<String> = match serde_json::from_raw_value(&req.params) {
                    Ok(n) => n,
                    Err(err) => {
                        self.write_error(ctx.conn_id, Some(req.id), JsonrpcErrorObj::unknown(err));
                        return;
                    }
                };
                if subscription_id.len() != 1 {
                    self.write_error(
                        ctx.conn_id,
                        Some(req.id),
                        JsonrpcErrorObj::unknown("invalid params"),
                    );
                    return;
                }
                match self.subscriptions.remove(&subscription_id[0]) {
                    Some(_) => {
                        glog::info!("remove subscription[unsubscribe]: {}", subscription_id[0]);
                        let result = serde_json::to_raw_value(&serde_json::Value::Null).unwrap();
                        self.write_response(
                            ctx.conn_id,
                            JsonrpcResponseRawResult::Ok(JsonrpcRawResponse {
                                jsonrpc: "2.0".into(),
                                result,
                                id: req.id,
                            }),
                        );
                        return;
                    }
                    None => {
                        self.write_error(
                            ctx.conn_id,
                            Some(req.id),
                            JsonrpcErrorObj::unknown("subscrption not found"),
                        );
                        return;
                    }
                }
            }
            _ => {
                self.write_error(
                    ctx.conn_id,
                    Some(req.id),
                    JsonrpcErrorObj::unknown("method not found"),
                );
                return;
            }
        }
    }
}
