use std::prelude::v1::*;

use apps::getargs::{Options, Opt};
use std::collections::BTreeMap;
use serde::Deserialize;
use eth_types::SU256;

#[derive(Deserialize, Debug, Clone)]
pub struct Config {
    // pub chain_id: SU256,
    // pub simulator_thread: usize,
    // pub trie_node_limit: usize,
    // pub tx_source: BTreeMap<String, String>,
    pub execution_nodes: Vec<String>,
    // pub tx_hashcache_size: usize,
    pub mev_boost_relay: mev_boost::Config,
}

#[derive(Debug)]
pub struct Args {
    pub executable: String,
    pub cfg: String,
    pub tx: String,
    pub block: u64,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            executable: "".into(),
            cfg: "config/pool-aggregator-mainnet.json".into(),
            tx: "".into(),
            block: 0,
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
                Opt::Short('c') => {
                    out.cfg = opts.value().unwrap().parse().unwrap();
                }
                Opt::Short('t') => {
                    out.tx = opts.value().unwrap().parse().unwrap();
                }
                Opt::Short('b') => {
                    out.block = opts.value().unwrap().parse().unwrap();
                }
                _ => continue,
            }
        }
        out
    }
}