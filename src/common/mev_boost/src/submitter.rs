use std::prelude::v1::*;
use std::time::Instant;

use base::format::parse_ether;
use base::time::Time;
use base::trace::Alive;
use blst::SecretKey;
use core::time::Duration;
use eth_client::BeaconSlot;
use eth_types::SH256;
use eth_types::SU256;
use net_http::HttpConnError;
use net_http::{HttpClient, HttpMethod, HttpRequestBuilder};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::mpsc;
use std::sync::Arc;
use std::sync::Mutex;

use crate::Domain;
use crate::Pubkey;
use crate::{BuilderSubmitBlockRequest, SubmitError};

#[derive(Debug, Clone)]
struct CheckedState {
    slot: u32,
    number: u64,
    randao: SH256,
    withdrawal_root: SH256,
    passed: Result<(), SubmitError>,
}

impl CheckedState {
    pub fn match_req(&self, req: &BuilderSubmitBlockRequest) -> bool {
        self.slot == req.message.slot.as_u32()
            && self.number == req.execution_payload.block_number.as_u64()
            && self.randao == req.execution_payload.prev_randao
            && self.withdrawal_root == req.withdrawal_root
    }
}

pub struct RelaySubmitter {
    name: String,
    is_optimistic: bool,
    endpoint: String,
    client: Arc<HttpClient>,
    signing_domain: Domain,
    test_key: SecretKey,
    beacon_slot: BeaconSlot,
    timeout: Option<Duration>,

    checked_state: Arc<Mutex<Vec<CheckedState>>>,
    history: Arc<Mutex<BTreeMap<u64, SU256>>>,
}

impl RelaySubmitter {
    pub fn new(
        name: String,
        endpoint: String,
        client: Arc<HttpClient>,
        signing_domain: Domain,
        test_key: SecretKey,
        beacon_slot: BeaconSlot,
        timeout: Option<Duration>,
    ) -> Self {
        Self {
            is_optimistic: name.ends_with("_optim"),
            name,
            endpoint,
            signing_domain,
            client,
            checked_state: Default::default(),
            test_key,
            beacon_slot,
            history: Default::default(),
            timeout,
        }
    }

    fn is_profit_smaller_than_history(&self, slot: u64, value: SU256) -> bool {
        let history = self.history.lock().unwrap();
        if let Some(val) = history.get(&slot) {
            if val >= &value {
                glog::info!(
                    "[slot={}] skip submitting block: new is {}, and the older is {}",
                    slot,
                    parse_ether(&value, 18),
                    parse_ether(&val, 18)
                );
                return true;
            }
        }
        return false;
    }

    pub fn listen(&self, alive: &Alive, receiver: &mpsc::Receiver<Arc<BuilderSubmitBlockRequest>>) {
        for mut req in alive.recv_iter(receiver, Duration::from_secs(1)) {
            let slot = req.message.slot.as_u64();
            let slot_info = format!("{}.{}", slot, req.execution_payload.block_number);
            let checked = self.checked(&req);
            if let Some(checked) = &checked {
                if checked.passed.is_err() {
                    glog::info!(
                        "[{}.{}] ignore submit due to wrong state {:?}",
                        checked.slot,
                        checked.number,
                        checked
                    );
                    continue;
                }
            }
            if self.is_profit_smaller_than_history(slot, req.message.value.into()) {
                continue;
            }

            let is_passed = checked.map(|n| n.passed.is_ok()).unwrap_or(false);
            if self.is_optimistic && !is_passed {
                // we need precheck
                let mut tmp = req.as_ref().clone();
                let key = &self.test_key;
                tmp.message.builder_pubkey = Pubkey::new(key.public().compress());
                tmp.signature = tmp.message.sign(self.signing_domain, &key);
                req = Arc::new(tmp);
            }
            base::thread::spawn(format!("MBR-{}-{}", slot_info, self.name), {
                let name = self.name.clone();
                let endpoint = self.endpoint.clone();
                let client = self.client.clone();
                let checked_state = self.checked_state.clone();
                let history = self.history.clone();
                let optimistic_enabled = self.is_optimistic && is_passed;
                let beacon_slot = self.beacon_slot.clone();
                let timeout = self.timeout.clone();
                move || {
                    let now = Instant::now();
                    let result = Self::submit_block(&endpoint, req.as_ref(), client, timeout);
                    let remain_time = beacon_slot
                        .time(req.message.slot.as_u64())
                        .duration_since(Time::now());
                    glog::info!(
                        "{}[{}] submit [{}.{}] dur={:?}, profit={:?}, result: {:?}, remain_time: {:?}",
                        if optimistic_enabled { "[OPTIM]" } else { "" },
                        name,
                        req.message.slot,
                        req.execution_payload.block_number,
                        now.elapsed(),
                        parse_ether(&req.message.value.into(), 18),
                        result,
                        remain_time,
                    );
                    {
                        let mut history = history.lock().unwrap();
                        let new_value = req.message.value.into();
                        let old_value = history.entry(slot).or_insert(new_value);
                        if new_value > *old_value {
                            *old_value = new_value;
                        }
                        if history.len() > 0 {
                            history.pop_first();
                        }
                    }
                    {
                        let mut checked_state = checked_state.lock().unwrap();
                        let mut matched = false;
                        for item in checked_state.iter_mut() {
                            if item.match_req(&req) {
                                item.passed = result.clone();
                                matched = true;
                                break;
                            }
                        }
                        if !matched {
                            checked_state.push(CheckedState {
                                slot: req.message.slot.as_u32(),
                                randao: req.execution_payload.prev_randao,
                                number: req.execution_payload.block_number.as_u64(),
                                withdrawal_root: req.withdrawal_root,
                                passed: result,
                            });
                            while checked_state.len() > 100 {
                                checked_state.remove(0);
                            }
                        }
                    }
                }
            });
        }
    }

    fn checked(&self, req: &BuilderSubmitBlockRequest) -> Option<CheckedState> {
        let states = self.checked_state.lock().unwrap();
        for item in states.iter() {
            if item.match_req(req) {
                return Some(item.clone());
            }
        }
        return None;
    }

    fn submit_block(
        endpoint: &str,
        req: &BuilderSubmitBlockRequest,
        client: Arc<HttpClient>,
        timeout: Option<Duration>,
    ) -> Result<(), SubmitError> {
        let url = format!("{}/relay/v1/builder/blocks", endpoint);
        let body = serde_json::to_vec(&req).unwrap();
        let mut http_req =
            HttpRequestBuilder::new(HttpMethod::Post, url.parse().unwrap(), Some(body));
        let response = client
            .send(&mut http_req, timeout)
            .map_err(|err| match err {
                HttpConnError::Timeout => SubmitError::Timeout,
                other => SubmitError::Unknown(format!("submit block fail {:?}", other)),
            })?;
        let data = response.body;
        if data.len() == 0 {
            return Ok(());
        }

        let val = match serde_json::from_slice(&data) {
            Ok(val) => val,
            Err(_) => {
                return Err(SubmitError::Unknown(
                    String::from_utf8_lossy(&data).into_owned(),
                ))
            }
        };

        if let Value::Object(val) = val {
            if val.len() == 0 {
                return Ok(());
            }
            if let Some(Value::String(info)) = val.get("message") {
                if info == "submission for past slot" {
                    return Err(SubmitError::SubmissionForPastSlot);
                }
                if info == "invalid signature" {
                    return Err(SubmitError::InvalidSignature);
                }
                if info == "payload for this slot was already delivered" {
                    return Err(SubmitError::SlotWasAlreadyDelivered);
                }
                if info == "payload attributes not (yet) known" {
                    return Err(SubmitError::SlotNotKnown(req.message.slot.as_u64()));
                }
                if info == "simulation failed: proposer payment not successful" {
                    return Err(SubmitError::ProposerPaymentNotSuccessful);
                }
                if info == "fee recipient does not match" {
                    return Err(SubmitError::FeeRecipientNotMatch(
                        req.message.proposer_fee_recipient,
                    ));
                }
                if info.starts_with("incorrect prev_randao") {
                    if let Some(idx) = info.find("expected:") {
                        let hash = info[idx + 9..].trim();
                        return Err(SubmitError::IncorrectPrevRandao(hash.into()));
                    }
                }
                if info.starts_with("incorrect withdrawals root") {
                    if let Some(idx) = info.find("expected:") {
                        let hash = info[idx + 9..].trim();
                        return Err(SubmitError::IncorrectWithdrawalsRoot(hash.into()));
                    }
                }
                return Err(SubmitError::Unknown(info.into()));
            }
        }

        return Err(SubmitError::Unknown(format!(
            "unknown error: {}",
            String::from_utf8_lossy(&data)
        )));
    }
}
