//! Routing core for allstops: the network model, earliest-arrival search
//! with visit tracking, plans, lower bounds and route search. Free of
//! filesystem and network access so it compiles unchanged to WebAssembly.

pub mod builder;
pub mod csa;
pub mod network;
pub mod oracle;
pub mod plan;
pub mod rules;

pub use network::{Network, Time};
