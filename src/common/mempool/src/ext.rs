use core::time::Duration;
use std::prelude::v1::*;

use crate::Client;
use base::thread::spawn;
use eth_client::{MempoolItem, TxFetcher, TxFetcherExtension, TxFetcherExtensionContext};
use eth_types::{Bundle, PoolTx};

pub struct WithMempool {
    id: u64,
}

impl WithMempool {
    pub fn new(id: u64) -> Box<dyn TxFetcherExtension> {
        Box::new(Self { id })
    }

    pub fn bind(fetcher: &mut TxFetcher, id: u64) {
        fetcher.add_ext("automata-mempool", move || Self::new(id));
    }
}

impl TxFetcherExtension for WithMempool {
    fn get_config(&self, alive: base::trace::Alive, endpoint: String) -> jsonrpc::WsClientConfig {
        let mut cfg = self.default_config(alive, endpoint);
        cfg.auto_resubscribe = true;
        cfg
    }

    fn run_in_background(&self, ctx: TxFetcherExtensionContext) {
        let name = ctx.client.tag.clone();
        let client = Client::new(self.id, ctx.client.clone(), ctx.signer.clone());
        spawn(format!("txfetcher-mempool-bundle-{}", name), {
            let client = client.clone();
            let ctx = ctx.clone();
            let name = name.clone();
            move || {
                while ctx.alive.is_alive() {
                    let pending_tx_sub = match client.subscribe_bundle() {
                        Ok(sub) => sub,
                        Err(err) => {
                            glog::error!("subscribe fail: {:?}", err);
                            base::thread::sleep_ms(1000);
                            continue;
                        }
                    };
                    loop {
                        let bundle = match pending_tx_sub.must_recv_within(Duration::from_secs(300))
                        {
                            Ok(tx) => tx,
                            Err(err) => {
                                glog::error!("err: {:?}", err);
                                break;
                            }
                        };
                        let bundle = match Bundle::from_rlp(&ctx.signer, bundle) {
                            Ok(n) => n,
                            Err(err) => {
                                glog::error!("{:?}", err);
                                continue;
                            }
                        };
                        let hash = bundle.hash();
                        if !ctx.hash_pool.first_seen(&hash) {
                            continue;
                        }
                        glog::debug!(target: "txpool", "[{}] receive bundle {:?}, len: {}", name, hash, bundle.txs.len());
                        ctx.counter.add();
                        if let Err(_) = ctx.sender.send(MempoolItem::Bundle(bundle)) {
                            break;
                        }
                    }
                }
            }
        });

        spawn(format!("txfetcher-mempool-ptx-{}", name), {
            let client = client.clone();
            move || {
                let txs = client.get_transaction().unwrap();
                for tx in txs {
                    if let Err(_) = ctx.sender.send(MempoolItem::Seq(tx)) {
                        break;
                    }
                }
                let pending_tx_sub = client.subscribe_tx().unwrap();
                loop {
                    let tx = match pending_tx_sub.must_recv_within(Duration::from_secs(300)) {
                        Ok(tx) => tx,
                        Err(err) => {
                            glog::error!("err: {:?}", err);
                            break;
                        }
                    };
                    let tx = match PoolTx::from_rlp(&ctx.signer, tx) {
                        Ok(n) => n,
                        Err(err) => {
                            glog::error!("{:?}", err);
                            continue;
                        }
                    };
                    if !ctx.hash_pool.first_seen(&tx.hash) {
                        continue;
                    }
                    glog::debug!(target: "txpool", "[{}] receive bundle {:?}, acl: {}", name, tx.hash, tx.access_list.len());
                    ctx.counter.add();
                    if let Err(_) = ctx.sender.send(MempoolItem::Seq(tx)) {
                        break;
                    }
                }
            }
        });
    }
}
