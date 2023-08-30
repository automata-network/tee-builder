use std::prelude::v1::*;

use apps::getargs::{Opt, Options};
use eth_types::{PoolItem, PoolItemType, SH256, SU256, SH160, HexBytes};
use serde::{Serialize, Deserialize};
use std::collections::BTreeMap;

#[derive(Debug, Deserialize)]
pub struct Config {
    pub chain_id: SU256,
    pub simulator_thread: usize,
    pub trie_node_limit: usize,
    pub tx_source: BTreeMap<String, String>,
    pub execution_node: String,
    pub tx_hashcache_size: usize,
    pub block_time: u64,
    pub genesis_time: u64,
    pub server: ServerConfig,
}

#[derive(Debug)]
pub struct Args {
    pub executable: String,
    pub port: u32,
    pub cfg: String,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            executable: "".into(),
            port: 18233,
            cfg: "".into(),
        }
    }
}

impl Args {
    pub fn from_args(mut args: Vec<String>) -> Self {
        let mut out = Args::default();
        out.executable = args.remove(0);
        let mut opts = Options::new(args.iter().map(|a| a.as_str()));
        while let Some(opt) = opts.next_opt().expect("argument parsing error") {
            match opt {
                Opt::Short('p') => {
                    out.port = opts.value().unwrap().parse().unwrap();
                }
                Opt::Short('c') => {
                    out.cfg = opts.value().unwrap().parse().unwrap();
                }
                _ => continue,
            }
        }
        out
    }
}

#[derive(Debug, Deserialize)]
pub struct ServerConfig {
    pub tls: String,
    pub body_limit: usize,
    pub workers: usize,
    pub redirect: String,
    pub forward_methods: Vec<String>,
    pub ias_spid: HexBytes,
    pub ias_apikey: String,
}

pub struct MemPoolItem {
    pub ty: PoolItemType,
    pub val: Option<PoolItem>,
}

impl From<PoolItemType> for MemPoolItem {
    fn from(ty: PoolItemType) -> Self {
        Self { ty, val: None }
    }
}

#[derive(Debug, Serialize, Clone, Default, Eq, PartialEq)]
pub struct PendingTransactionInfo {
    pub hash: SH256,
    pub nonce: u64,
    pub caller: SH160,
    pub to: Option<SH160>,
    pub value: SU256,
    // pub reason: Option<String>,
    pub submit_time: String,
    // pub propose_to: Vec<u64>,
}

#[derive(Debug, Serialize, Clone, Default, Eq, PartialEq)]
pub struct StatBundle {
    pub hash: SH256,
    pub block_number: u64,
    pub status: String,
    pub num_txs: usize,
}