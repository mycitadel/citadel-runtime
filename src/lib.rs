// Citadel: Bitcoin, LN & RGB wallet runtime
// Written in 2021 by
//     Dr. Maxim Orlovsky <orlovsky@mycitadel.io>
//
// To the extent possible under law, the author(s) have dedicated all
// copyright and related and neighboring rights to this software to
// the public domain worldwide. This software is distributed without
// any warranty.
//
// You should have received a copy of the AGPL License
// along with this software.
// If not, see <https://www.gnu.org/licenses/agpl-3.0-standalone.html>.

#![recursion_limit = "256"]
// Coding conventions
#![deny(
    non_upper_case_globals,
    non_camel_case_types,
    non_snake_case,
    unused_mut,
    unused_imports,
    // dead_code
    // missing_docs,
)]
#![allow(dead_code)]
#![allow(unused_variables)]

#[macro_use]
extern crate amplify;
#[macro_use]
extern crate amplify_derive;
#[macro_use]
extern crate lnpbp;
#[macro_use]
extern crate internet2;

#[macro_use]
extern crate log;

#[macro_use]
extern crate serde_with;

mod error;
pub mod model;
#[cfg(any(feature = "client", feature = "runtime"))]
pub mod rpc;

#[cfg(feature = "client")]
pub mod client;
#[cfg(all(feature = "client", feature = "runtime"))]
mod embedded;
#[cfg(all(feature = "client", feature = "runtime"))]
pub use embedded::run_embedded;
#[cfg(feature = "runtime")]
pub mod runtime;

pub mod cache;
pub mod storage;

#[cfg(feature = "runtime")]
pub mod chainapi;
#[cfg(feature = "runtime")]
pub mod chainwatch;

#[cfg(feature = "client")]
pub use client::Client;
pub use error::Error;
