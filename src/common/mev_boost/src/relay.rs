use std::prelude::v1::*;

use crate::{
    BidTrace, BuilderSubmitBlockRequest, Config, Domain, Error, ExecutionPayload, Pubkey,
    RelaySubmitter,
};
use base::format::parse_ether;
use base::trace::Alive;
use eth_client::BeaconSlot;
use eth_types::{deserialize_u32, Block, HexBytes, SH160, SU256, SU64, SH256};
use net_http::{HttpClient, HttpMethod, HttpRequestBuilder};
use serde::Deserialize;
use std::collections::{btree_map::Entry, BTreeMap};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use std::time::Instant;

use blst::SecretKey;

pub struct Relay {
    signer_sk: SecretKey,
    signer: Pubkey,
    sign_domain: Domain,

    validators: Arc<Mutex<RelayValidatorMap>>,
    senders: Vec<(String, mpsc::SyncSender<Arc<BuilderSubmitBlockRequest>>)>,
}

impl Relay {
    pub fn new(alive: Alive, cfg: Config, beacon_slot: BeaconSlot) -> Self {
        let signer_sk = cfg.signer.0;
        let test_key = cfg.test_signer.0;
        let signer = Pubkey::new(signer_sk.public().compress());
        let test_signer = Pubkey::new(test_key.public().compress());
        glog::info!(
            "relay account: [prod] {}, [test] {}",
            HexBytes::from(signer.as_bytes()),
            HexBytes::from(test_signer.as_bytes())
        );
        let sign_domain = Domain::new([0, 0, 0, 1], cfg.genesis_fork_version.clone());

        let validator_map = Arc::new(Mutex::new(RelayValidatorMap::new()));
        let mut senders = Vec::with_capacity(cfg.endpoints.len());
        let client = Arc::new(HttpClient::new());
        for (name, endpoint) in &cfg.endpoints {
            if name.starts_with("!") {
                continue;
            }
            base::thread::spawn(format!("relay-{}", name), {
                let (sender, receiver) = mpsc::sync_channel(1);
                let block_submitter = RelaySubmitter::new(
                    name.clone(),
                    endpoint.clone(),
                    client.clone(),
                    sign_domain.clone(),
                    test_key.clone(),
                    beacon_slot.clone(),
                    cfg.submit_timeout.map(|t| Duration::from_millis(t)),
                );
                senders.push((name.clone(), sender));
                let alive = alive.clone();
                move || block_submitter.listen(&alive, &receiver)
            });

            base::thread::spawn(format!("relay-{}", name), {
                let name = name.clone();
                let client = client.clone();
                let endpoint = endpoint.clone();
                let validator_map = validator_map.clone();
                let alive = alive.clone();
                move || loop {
                    Self::fetch_and_update_validator(&name, &client, &endpoint, &validator_map);
                    if !alive.sleep_ms(1000) {
                        break;
                    }
                }
            });
        }

        let relay = Self {
            sign_domain,
            signer_sk,
            signer,
            validators: validator_map.clone(),
            senders,
        };
        relay
    }

    fn fetch_and_update_validator(
        name: &str,
        client: &HttpClient,
        endpoint: &str,
        validator_map: &Mutex<RelayValidatorMap>,
    ) {
        match Self::fetch_validator(name, &endpoint, client) {
            Ok(response) => {
                let mut map = validator_map.lock().unwrap();
                match map.update(name, &response) {
                    Ok(updated) => {
                        if updated {
                            let slots = response.iter().map(|item| item.slot).collect::<Vec<_>>();
                            glog::info!("[{}] updated validator map: {:?}", name, slots);
                        }
                    }
                    Err(err) => {
                        glog::error!("update validator map fail: {:?}", err);
                    }
                }
            }
            Err(err) => {
                glog::error!("fetch validator data fail: [{}] {:?}", name, err);
            }
        }
    }

    fn fetch_best_bid(
        name: &str,
        endpoint: &str,
        client: &HttpClient,
        slot: u64,
        parent_hash: SH256,
        pubkey: HexBytes,
    ) -> Result<(), Error> {
        let uri = format!(
            "{}/eth/v1/builder/header/{}/{:?}/{}",
            endpoint, slot, parent_hash, pubkey
        );

        glog::info!("fetch topbid: {}", uri);

        let mut req = HttpRequestBuilder::new(HttpMethod::Get, uri.parse().unwrap(), None);
        let response = client
            .send(&mut req, Some(Duration::from_secs(1)))
            .map_err(|err| Error::FetchValidatorFail(format!("[{}] {:?}", name, err)))?;
        glog::info!("response: {}", String::from_utf8_lossy(&response.body));
        Ok(())
    }

    fn fetch_validator(
        name: &str,
        endpoint: &str,
        client: &HttpClient,
    ) -> Result<Vec<GetValidatorRelayResponseItem>, Error> {
        let uri = format!("{}/relay/v1/builder/validators", endpoint)
            .parse()
            .unwrap();
        let mut req = HttpRequestBuilder::new(HttpMethod::Get, uri, None);
        let response = client
            .send(&mut req, Some(Duration::from_secs(1)))
            .map_err(|err| Error::FetchValidatorFail(format!("[{}] {:?}", name, err)))?;
        serde_json::from_slice(&response.body).map_err(|err| {
            Error::FetchValidatorFail(format!(
                "[{}]{}: {}",
                name,
                err,
                String::from_utf8_lossy(&response.body)
            ))
        })
    }

    fn build_request(
        &self,
        slot: u64,
        vd: &ValidatorData,
        value: SU256,
        blk: &Block,
    ) -> BuilderSubmitBlockRequest {
        let payload: ExecutionPayload = blk.into();
        let trace = BidTrace {
            slot: slot.into(),
            parent_hash: payload.parent_hash,
            block_hash: payload.block_hash,
            builder_pubkey: self.signer.clone(),
            proposer_pubkey: vd.pub_key.as_bytes().into(),
            proposer_fee_recipient: vd.fee_recipient,
            gas_limit: payload.gas_limit,
            gas_used: payload.gas_used,
            value: value.into(),
        };
        BuilderSubmitBlockRequest {
            signature: trace.sign(self.sign_domain, &self.signer_sk),
            message: trace,
            execution_payload: payload,
            withdrawal_root: blk.header.withdrawals_root.unwrap(),
        }
    }

    pub fn submit_block(&self, slot: u64, vd: &ValidatorData, blk: &Block, value: SU256) {
        let now = Instant::now();
        let req = Arc::new(self.build_request(slot, vd, value, blk));
        for (name, sender) in &self.senders {
            if !vd.name.contains(name) {
                continue;
            }
            let _ = sender.send(req.clone());
        }

        glog::info!(
            "[{}] submit result: to {:?}, recipient: {:?}, profit: {}, elapsed: {:?}",
            req.execution_payload.block_number,
            vd.name,
            req.message.proposer_fee_recipient,
            parse_ether(&req.message.value.into(), 18),
            now.elapsed(),
        );
    }

    pub fn get_validator_for_slot(&self, next_slot: u64) -> Result<ValidatorData, Error> {
        let guard = self.validators.lock().unwrap();
        guard.try_get(next_slot)
    }
}

#[derive(Default, Clone, Debug)]
pub struct RelayValidatorMap {
    data: BTreeMap<u64, ValidatorData>,
}

impl RelayValidatorMap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn try_get(&self, slot: u64) -> Result<ValidatorData, Error> {
        if let Some(n) = self.data.get(&slot) {
            return Ok(n.clone());
        }
        for (s, _) in &self.data {
            if *s > slot {
                return Err(Error::NoValidatorInSlot {
                    current: slot,
                    next: *s,
                });
            }
        }
        return Err(Error::NoValidatorInSlot {
            current: slot,
            next: 0,
        });
    }

    fn update(
        &mut self,
        name: &str,
        response: &[GetValidatorRelayResponseItem],
    ) -> Result<bool, Error> {
        let name = name.to_owned();
        let mut updated = false;
        for item in response {
            match self.data.entry(item.slot as u64) {
                Entry::Occupied(mut entry) => {
                    let vd = entry.get_mut();
                    if !vd.name.contains(&name) {
                        vd.name.push(name.clone());
                        updated = true;
                    }
                }
                Entry::Vacant(entry) => {
                    entry.insert(ValidatorData {
                        pub_key: HexBytes::from_hex(item.entry.message.pubkey.as_bytes())
                            .map_err(|_| Error::InvalidValidatorPubkey)?,
                        fee_recipient: item.entry.message.fee_recipient,
                        gas_limit: item.entry.message.gas_limit.as_u64(),
                        timestamp: item.entry.message.timestamp.as_u64(),
                        name: vec![name.clone()],
                    });
                    updated = true;
                }
            };
        }
        while self.data.len() > 64 {
            self.data.pop_first();
        }
        Ok(updated)
    }
}

#[derive(Clone, Debug)]
pub struct ValidatorData {
    pub pub_key: HexBytes,
    pub fee_recipient: SH160,
    pub gas_limit: u64,
    pub timestamp: u64,
    pub name: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct GetValidatorRelayResponseItem {
    #[serde(deserialize_with = "deserialize_u32")]
    slot: u32,
    entry: GetValidatorRelayResponseItemEntry,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct GetValidatorRelayResponseItemEntry {
    message: GetValidatorRelayResponseItemEntryMessage,
    signature: String,
}

#[derive(Debug, Deserialize)]
struct GetValidatorRelayResponseItemEntryMessage {
    fee_recipient: SH160,
    gas_limit: SU64,
    timestamp: SU64,
    pubkey: String,
}
