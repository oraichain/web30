#![warn(clippy::all)]
#![allow(clippy::large_enum_variant)]
#![allow(clippy::pedantic)]

#[macro_use]
extern crate serde_derive;
#[macro_use]
extern crate log;
#[macro_use]
extern crate lazy_static;

pub mod amm;
pub mod client;
mod erc20_utils;
mod erc721_utils;
pub mod eth_wrapping;
mod event_utils;
pub mod gas_estimator;
pub mod jsonrpc;
mod mem;
mod tron_utils;
pub mod types;

pub use clarity::Address as EthAddress;
pub use event_utils::address_to_event;
pub use event_utils::{ContractEvent, Web3Event};
pub use heliosphere::core::{event::EventData, Address as TronAddress};
