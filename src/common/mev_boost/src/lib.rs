#![cfg_attr(not(feature = "std"), no_std)]
#![feature(map_first_last)]

#[cfg(feature = "tstd")]
#[macro_use]
extern crate sgxlib as std;

mod relay;
pub use relay::*;
mod types;
pub use types::*;
mod submitter;
pub use submitter::*;