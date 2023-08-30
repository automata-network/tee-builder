#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "tstd")]
#[macro_use]
extern crate sgxlib as std;

mod types;
pub use types::*;
mod client;
pub use client::*;

pub mod ext;