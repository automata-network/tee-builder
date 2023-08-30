use std::prelude::v1::*;

use eth_types::SU64;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GetBundleRequest {
    pub block_number: SU64,
    pub timestamp: Option<SU64>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct GetTxRequest {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SubscribeOpt {
    NewBundle,
    NewTx,
}
