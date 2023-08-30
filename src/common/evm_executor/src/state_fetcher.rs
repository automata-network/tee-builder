use std::prelude::v1::*;

use std::sync::Arc;

use base::format::debug;
use base::trace::AvgCounter;
use eth_client::ExecutionClient;
use eth_types::{
    BlockSelector, FetchState, FetchStateResult, HexBytes, TransactionAccessTuple, H160, H256,
    SH160, SH256, SU256,
};
use std::borrow::Cow;

#[derive(Clone, Debug)]
pub struct BlockStateFetcher {
    client: Arc<ExecutionClient>,
    blk: BlockSelector,
    acc: Option<SH160>,
    counter: AvgCounter,
}

impl BlockStateFetcher {
    pub fn new(client: Arc<ExecutionClient>, blk: BlockSelector) -> BlockStateFetcher {
        Self {
            client,
            acc: None,
            blk,
            counter: AvgCounter::new(),
        }
    }
}

impl statedb::StateFetcher for BlockStateFetcher {
    fn with_acc(&self, address: &SH160) -> Self {
        Self {
            client: self.client.clone(),
            blk: self.blk.clone(),
            acc: Some(address.clone()),
            counter: self.counter.clone(),
        }
    }

    fn fork(&self) -> Self {
        self.clone()
    }

    fn get_block_hash(&self, number: u64) -> Result<SH256, statedb::Error> {
        let _counter = self.counter.place();

        let header = self
            .client
            .get_block_header(number.into())
            .map_err(|err| statedb::Error::CallRemoteFail(format!("[get_block_hash] {:?}", err)))?;
        Ok(header.hash())
    }

    fn get_account(&self, address: &SH160) -> Result<(SU256, u64, HexBytes), statedb::Error> {
        let _counter = self.counter.place();

        let fetch_state = FetchState {
            access_list: Some(Cow::Owned(TransactionAccessTuple {
                address: address.clone(),
                storage_keys: Vec::new(),
            })),
            code: Some(address.clone()),
        };
        let result = self
            .client
            .fetch_states(&[fetch_state], self.blk, false)
            .map_err(|err| statedb::Error::CallRemoteFail(format!("{:?}", err)))?
            .pop()
            .unwrap();
        let acc = result.acc.unwrap();
        Ok((acc.balance, acc.nonce.as_u64(), result.code.unwrap()))
    }

    fn get_storage(&self, address: &SH160, key: &SH256) -> Result<SH256, statedb::Error> {
        let _counter = self.counter.place();

        Ok(self
            .client
            .get_storage(address, key, self.blk)
            .map_err(|err| statedb::Error::CallRemoteFail(format!("{:?}", err)))?)
    }

    fn get_code(&self, address: &SH160) -> Result<HexBytes, statedb::Error> {
        let _counter = self.counter.place();

        let code = self
            .client
            .get_code(address, self.blk)
            .map_err(|err| statedb::Error::CallRemoteFail(format!("[get_block_hash] {:?}", err)))?;
        Ok(code)
    }

    fn prefetch_states(
        &self,
        list: &[FetchState],
        with_proof: bool,
    ) -> Result<Vec<FetchStateResult>, statedb::Error> {
        self.client
            .fetch_states(list, self.blk, with_proof)
            .map_err(|err| statedb::Error::CallRemoteFail(format!("[get_block_hash] {:?}", err)))
    }

    fn get_miss_usage(&self) -> base::trace::AvgCounterResult {
        self.counter.take()
    }
}

impl statedb::ProofFetcher for BlockStateFetcher {
    fn fetch_proofs(&self, key: &[u8]) -> Result<Vec<HexBytes>, String> {
        let _counter = self.counter.place();
        glog::debug!(exclude: "dry_run", target: "state_fetch", "fetch proof: acc[{:?}] {}", self.acc, HexBytes::from(key));
        match &self.acc {
            Some(acc) => {
                assert_eq!(key.len(), 32);
                let key = H256::from_slice(key).into();
                let result = self
                    .client
                    .get_proof(acc, &[key], self.blk)
                    .map_err(debug)?;
                let storage = result.storage_proof.into_iter().next().unwrap();
                Ok(storage.proof)
            }
            None => {
                assert_eq!(key.len(), 20);
                let account = H160::from_slice(key).into();
                let result = self
                    .client
                    .get_proof(&account, &[], self.blk)
                    .map_err(debug)?;
                Ok(result.account_proof)
            }
        }
    }

    fn get_nodes(&self, node: &[SH256]) -> Result<Vec<HexBytes>, String> {
        let _counter = self.counter.place();

        self.client
            .get_dbnodes(node)
            .map_err(|err| format!("{:?}", err))
    }
}
