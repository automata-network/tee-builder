use core::sync::atomic::Ordering;
use std::{prelude::v1::*, sync::Mutex};

use base::format::debug;
use base::trace::Alive;
use crypto::Aes128EncryptedMsg;
use crypto::{Aes128Key, Sr25519PublicKey};
use eth_types::{BundleRlp, PoolTx, PoolTxRlp, Signer};
use jsonrpc::RpcEncrypt;
use jsonrpc::{JsonrpcClient, JsonrpcWsClient, RpcError, WsClientError, WsSubscription};
use serde::de::DeserializeOwned;
use serde::Serialize;
use std::sync::atomic::AtomicUsize;
use std::sync::Arc;

use crate::{GetTxRequest, SubscribeOpt};

#[derive(Default)]
pub struct AuthInfo {
    pub pubkey: Sr25519PublicKey,
}

#[derive(Clone)]
pub struct Client {
    id: u64,
    client: JsonrpcClient<JsonrpcWsClient>,
    signer: Signer,
    epoch: Arc<AtomicUsize>,

    #[cfg(feature = "sgx")]
    key: Arc<Mutex<Option<sgxlib_ra::ExchangeResult>>>,
}

impl Client {
    pub fn new(id: u64, client: JsonrpcWsClient, signer: Signer) -> Self {
        Self {
            id,
            epoch: Arc::new(AtomicUsize::new(0)),
            client: JsonrpcClient::new(client),
            signer,

            #[cfg(feature = "sgx")]
            key: Arc::new(Mutex::new(None)),
        }
    }

    fn subscribe<T, E>(
        &self,
        method: &str,
        unsub_method: &str,
        params: E,
    ) -> Result<WsSubscription<T>, RpcError>
    where
        T: DeserializeOwned + 'static,
        E: Serialize + std::fmt::Debug,
    {
        let auth = self.check_auth()?;
        let mut sub: WsSubscription<T> = self
            .client
            .raw()
            .subscribe(method, unsub_method, params)
            .map_err(|err| RpcError::RecvResponseError(format!("{:?}", err)))?;
        #[cfg(feature = "sgx")]
        {
            sub = sub.with_formatter(move |value| {
                let val: Aes128EncryptedMsg = WsSubscription::json_formatter(value)?;
                auth.decrypt(&auth.pubkey, val)
                    .map_err(|err| WsClientError::SubscribeResponse(err))
            });
        }
        Ok(sub)
    }

    pub fn subscribe_bundle(&self) -> Result<WsSubscription<BundleRlp>, RpcError> {
        let auth = self.check_auth()?;
        self.subscribe(
            "pool_subscribe",
            "pool_unsubscribe",
            &(&auth.pubkey, SubscribeOpt::NewBundle),
        )
    }

    pub fn subscribe_tx(&self) -> Result<WsSubscription<PoolTxRlp>, RpcError> {
        let auth = self.check_auth()?;
        self.subscribe(
            "pool_subscribe",
            "pool_unsubscribe",
            &(&auth.pubkey, SubscribeOpt::NewTx),
        )
    }

    pub fn get_transaction(&self) -> Result<Vec<PoolTx>, RpcError> {
        let req = GetTxRequest {};
        let response: Vec<PoolTxRlp> = self.rpc("pool_getRawTransaction", &(req,)).unwrap();
        glog::info!("get transaction: {:?}", response);
        let mut out = Vec::with_capacity(response.len());
        for item in response {
            out.push(
                PoolTx::from_rlp(&self.signer, item)
                    .map_err(|err| RpcError::RecvResponseError(debug(err)))?,
            );
        }
        Ok(out)
    }

    #[cfg(not(feature = "sgx"))]
    fn check_auth(&self) -> Result<AuthInfo, RpcError> {
        Ok(AuthInfo::default())
    }

    #[cfg(feature = "sgx")]
    fn check_auth(&self) -> Result<sgxlib_ra::ExchangeResult, RpcError> {
        let mut key = self.key.lock().unwrap();

        let epoch = self.client.raw().epoch();
        let old_epoch = self.epoch.load(Ordering::SeqCst);
        glog::info!("epoch: {} {}", epoch, old_epoch);
        if epoch == old_epoch {
            if let Some(key) = key.as_ref() {
                return Ok(key.clone());
            }
        }

        let result = sgxlib_ra::exchange_key(self.id, &self.client.raw()).unwrap();
        *key = Some(result.clone());

        self.epoch.store(epoch, Ordering::SeqCst);
        Ok(result)
    }

    #[cfg(not(feature = "sgx"))]
    fn rpc<P, R>(&self, method: &str, params: P) -> Result<R, RpcError>
    where
        P: Serialize + std::fmt::Debug,
        R: DeserializeOwned,
    {
        self.client.rpc(method, params)
    }

    #[cfg(feature = "sgx")]
    fn rpc<P, R>(&self, method: &str, params: P) -> Result<R, RpcError>
    where
        P: Serialize + std::fmt::Debug,
        R: DeserializeOwned,
    {
        let result = self.check_auth()?;
        self.client.enc_rpc(&result, &result.pubkey, method, params)
    }
}
