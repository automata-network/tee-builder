use std::prelude::v1::*;

use std::collections::BTreeMap;

use blst::SecretKey;
use eth_types::HexBytes;
use eth_types::{serialize_u256, serialize_u64, Block};
use eth_types::{SH160, SH256, SU256, U256, U64};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
pub struct Config {
    pub signer: BlstSecretKey,
    pub test_signer: BlstSecretKey,
    pub endpoints: BTreeMap<String, String>,
    pub genesis_fork_version: HexBytes,
    pub submit_time_millis: u64,
    pub timeout: Option<u64>,
    pub submit_timeout: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Eq, PartialEq)]
pub struct BuilderSubmitBlockRequest {
    pub signature: HexBytes,
    pub message: BidTrace,
    pub execution_payload: ExecutionPayload,

    #[serde(skip)]
    pub withdrawal_root: SH256, // cache from blockHeader
}

#[derive(Debug, Serialize, Clone, Eq, PartialEq)]
pub struct BidTrace {
    #[serde(serialize_with = "serialize_u64")]
    pub slot: U64,
    pub parent_hash: SH256,
    pub block_hash: SH256,
    pub builder_pubkey: Pubkey,
    pub proposer_pubkey: Pubkey,
    pub proposer_fee_recipient: SH160,
    #[serde(serialize_with = "serialize_u64")]
    pub gas_limit: U64,
    #[serde(serialize_with = "serialize_u64")]
    pub gas_used: U64,

    #[serde(serialize_with = "serialize_u256")]
    pub value: U256,
}

impl ssz::HashTree for BidTrace {
    fn hash_tree_root_with(&self, h: &mut ssz::Hasher) -> Result<(), ssz::Error> {
        h.put_u64(self.slot.as_u64());
        h.put_bytes(self.parent_hash.as_bytes());
        h.put_bytes(self.block_hash.as_bytes());
        h.put_bytes(self.builder_pubkey.as_bytes());
        h.put_bytes(self.proposer_pubkey.as_bytes());
        h.put_bytes(self.proposer_fee_recipient.as_bytes());
        h.put_u64(self.gas_limit.as_u64());
        h.put_u64(self.gas_used.as_u64());
        let mut n = [0_u8; 32];
        self.value.to_little_endian(&mut n);
        h.put_bytes(&n);
        Ok(())
    }
}

impl BidTrace {
    pub fn sign(&self, signing_domain: Domain, sk: &blst::SecretKey) -> HexBytes {
        let root = signing_domain.sign_hash_root(self);
        let signature_bytes = blst::sign(sk, &root[..]).compress();
        HexBytes::from(&signature_bytes[..])
    }
}

#[derive(Debug, Serialize, Clone, Eq, PartialEq)]
pub struct ExecutionPayload {
    pub parent_hash: SH256,
    pub fee_recipient: SH160,
    pub state_root: SH256,
    pub receipts_root: SH256,
    pub prev_randao: SH256,
    pub logs_bloom: HexBytes,
    #[serde(serialize_with = "serialize_u64")]
    pub block_number: U64,
    #[serde(serialize_with = "serialize_u64")]
    pub gas_limit: U64,
    #[serde(serialize_with = "serialize_u64")]
    pub gas_used: U64,
    #[serde(serialize_with = "serialize_u64")]
    pub timestamp: U64,
    pub extra_data: HexBytes,
    pub base_fee_per_gas: SU256,
    pub block_hash: SH256,
    pub transactions: Vec<HexBytes>,
    pub withdrawals: Vec<Withdrawal>,
}

impl From<&Block> for ExecutionPayload {
    fn from(blk: &Block) -> Self {
        let block_hash = blk.header.hash();
        let mut transactions = Vec::new();
        for tx in &blk.transactions {
            match tx.clone().inner() {
                Some(inner) => {
                    transactions.push(inner.to_bytes().into());
                }
                None => continue,
            }
        }
        let mut logs_bloom = blk.header.logs_bloom.clone().into_vec();
        let length = logs_bloom.len() - 256;
        logs_bloom.rotate_left(length);
        let withdrawals = blk
            .withdrawals
            .as_ref()
            .map(|item| item.iter().map(|n| n.clone().into()).collect())
            .unwrap();

        Self {
            parent_hash: blk.header.parent_hash,
            fee_recipient: blk.header.miner,
            state_root: blk.header.state_root,
            receipts_root: blk.header.receipts_root,
            logs_bloom: logs_bloom.into(),
            prev_randao: blk.header.mix_hash,
            block_number: blk.header.number.into(),
            gas_limit: blk.header.gas_limit.into(),
            gas_used: blk.header.gas_used.into(),
            timestamp: blk.header.timestamp.into(),
            extra_data: blk.header.extra_data.clone(),
            base_fee_per_gas: blk.header.base_fee_per_gas,
            block_hash,
            transactions,
            withdrawals,
        }
    }
}

#[derive(Debug, Serialize, Clone, Eq, PartialEq)]
pub struct Withdrawal {
    #[serde(serialize_with = "serialize_u64")]
    pub index: U64,
    #[serde(serialize_with = "serialize_u64")]
    pub validator_index: U64,
    pub address: SH160,
    #[serde(serialize_with = "serialize_u64")]
    pub amount: U64,
}

impl From<eth_types::Withdrawal> for Withdrawal {
    fn from(old: eth_types::Withdrawal) -> Self {
        Self {
            index: old.index.into(),
            validator_index: old.validator_index.into(),
            address: old.address.clone(),
            amount: old.amount.into(),
        }
    }
}

#[derive(Debug)]
pub enum Error {
    InvalidDomain(String),
    SubmitError(SubmitError),
    NoValidatorInSlot { current: u64, next: u64 },
    InvalidValidatorPubkey,
    FetchValidatorFail(String),
}

#[derive(Debug, Clone)]
pub enum SubmitError {
    SubmissionForPastSlot,
    SlotWasAlreadyDelivered,
    InvalidSignature,
    ProposerPaymentNotSuccessful,
    SlotNotKnown(u64),
    FeeRecipientNotMatch(SH160),
    IncorrectPrevRandao(SH256),
    IncorrectWithdrawalsRoot(SH256),
    NoPrevRandao,
    Timeout,
    Unknown(String),
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub struct Pubkey([u8; 48]);
// ssz::impl_type!(Pubkey, [u8; 48]);

impl Pubkey {
    pub fn new(val: [u8; 48]) -> Self {
        Self(val)
    }

    pub fn as_bytes(&self) -> &[u8] {
        &self.0[..]
    }
}

impl<'de> Deserialize<'de> for Pubkey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        // let str = String::deserialize(deserializer)?;
        let data: HexBytes = HexBytes::deserialize(deserializer)?;
        Ok(data.as_bytes().into())
    }
}

impl Serialize for Pubkey {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let val = format!("0x{}", hex::encode(&self.0));
        serializer.serialize_str(&val)
    }
}

impl Default for Pubkey {
    fn default() -> Self {
        Self(unsafe { std::mem::zeroed() })
    }
}

impl From<Vec<u8>> for Pubkey {
    fn from(v: Vec<u8>) -> Self {
        v.as_slice().into()
    }
}

impl From<&[u8]> for Pubkey {
    fn from(v: &[u8]) -> Self {
        let mut val = Self::default();
        val.0.copy_from_slice(v);
        val
    }
}

impl From<[u8; 48]> for Pubkey {
    fn from(v: [u8; 48]) -> Self {
        Self(v)
    }
}

impl From<&Pubkey> for [u8; 48] {
    fn from(v: &Pubkey) -> Self {
        v.0
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct Domain([u8; 32]);

impl Domain {
    pub fn new(domain_type: [u8; 4], genesis_fork_version: HexBytes) -> Self {
        if genesis_fork_version.len() != 4 {
            panic!("invalid genesis fork version");
        }
        let mut current_version = [0_u8; 4];
        current_version.copy_from_slice(&genesis_fork_version);

        let fork_data = ForkData {
            current_version,
            genesis_validators_root: SH256::default(),
        };
        let fork_data_root = ssz::hash_root(&fork_data).expect("hash fork data fail");

        let mut domain = [0_u8; 32];
        domain[..4].copy_from_slice(&domain_type);
        domain[4..].copy_from_slice(&fork_data_root[..28]);
        Self(domain)
    }

    pub fn sign_hash_root<T: ssz::HashTree>(&self, obj: &T) -> [u8; 32] {
        // let zero = [0_u8; 32];
        let root = ssz::hash_root(obj).expect("hash root fail");
        let signing_data = SigningData {
            root: root.into(),
            domain: self.clone(),
        };
        ssz::hash_root(&signing_data).unwrap()
    }
}

pub struct ForkData {
    current_version: [u8; 4],
    genesis_validators_root: SH256,
}

impl ssz::HashTree for ForkData {
    fn hash_tree_root_with(&self, h: &mut ssz::Hasher) -> Result<(), ssz::Error> {
        h.put_bytes(&self.current_version);
        h.put_bytes(self.genesis_validators_root.as_bytes());
        Ok(())
    }
}

pub struct SigningData {
    root: SH256,
    domain: Domain,
}

impl ssz::HashTree for SigningData {
    fn hash_tree_root_with(&self, h: &mut ssz::Hasher) -> Result<(), ssz::Error> {
        h.put_bytes(&self.root.as_bytes());
        h.put_bytes(&self.domain.0);
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BlstSecretKey(pub SecretKey);

impl<'de> Deserialize<'de> for BlstSecretKey {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        use serde::de::Error;
        let str = String::deserialize(deserializer)?;
        let result: Vec<u8> = if str.starts_with("0x") {
            hex::decode(&str[2..]).map_err(|e| D::Error::custom(format!("{}", e)))?
        } else {
            str.into()
        };
        if result.len() != 32 {
            return Err(D::Error::custom(format!("invalid sk length")));
        }

        let key =
            SecretKey::from_bytes(&result).map_err(|err| D::Error::custom(format!("{:?}", err)))?;
        Ok(Self(key))
    }
}