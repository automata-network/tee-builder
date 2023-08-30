use std::prelude::v1::*;

use apps::{
    getargs::{Opt, Options},
    AppEnv,
};

use serde::Deserialize;
use std::collections::BTreeMap;

#[derive(Deserialize, Debug, Clone)]
pub struct Config {
    pub mev_boost_relay: mev_boost::Config,
    pub execution_nodes: Vec<String>,
    pub builder: block_builder::Config,
    pub tx_source: BTreeMap<String, String>,
    pub simulator_thread: usize,
    pub beacon_endpoint: String,
    pub trie_store_size: usize,
    pub txpool_size: usize,
    pub block_time: u64,
    pub tx_hashcache_size: usize,
    pub mempool_signer: Option<crypto::Secp256k1PrivateKey>,
    pub server: ServerConfig,
    pub disable_build: bool,
}

impl Config {
    pub fn init(&mut self) {
        self.builder.init();
    }

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

    pub enclave_id: u64,
}

impl Default for Args {
    fn default() -> Self {
        Self {
            executable: "".into(),
            port: 18231,
            cfg: "config.json".into(),
            enclave_id: 0,
        }
    }
}

impl Args {
    pub fn from_args(mut env: AppEnv) -> Self {
        let mut out = Args::default();
        out.executable = env.args.remove(0);
        let mut opts = Options::new(env.args.iter().map(|a| a.as_str()));
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
        out.enclave_id = env.enclave_id;
        out
    }
}

#[derive(Deserialize, Debug, Clone)]
pub struct ServerConfig {
    pub tls: String,
    pub body_limit: usize,
    pub workers: usize,
    pub public_methods: Vec<String>,
    pub redirect: String,
}
