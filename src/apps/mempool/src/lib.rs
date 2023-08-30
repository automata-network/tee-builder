#![cfg_attr(not(feature = "std"), no_std)]
#![feature(int_abs_diff)]

#[cfg(feature = "tstd")]
#[macro_use]
extern crate sgxlib as std;

pub use apps;

mod app;
pub use app::*;
mod types;
pub use types::*;
mod api;
pub use api::*;