use std::prelude::v1::*;

use apps::getargs::{Opt, Options};
use eth_types::SU256;
use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize, Debug, Clone)]
pub struct Config {
    pub chain_id: SU256,
    pub simulator_thread: usize,
    pub trie_node_limit: usize,
    pub tx_source: BTreeMap<String, String>,
    pub execution_nodes: Vec<String>,
    pub tx_hashcache_size: usize,
    pub block_time: u64,
    pub genesis_time: u64,
}

impl Config {
    pub fn get_el_endpoint(&self, filter: &str) -> Option<String> {
        self.execution_nodes
            .iter()
            .find(|n| n.starts_with(filter))
            .cloned()
    }
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
            port: 18232,
            cfg: "config/pool-aggregator.json".into(),
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
