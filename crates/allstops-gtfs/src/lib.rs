//! GTFS loading for allstops: input limits, service calendars, station
//! clustering, target selection and footpaths. No filesystem or network
//! access; callers pass bytes in, so the crate can target WebAssembly.

pub mod archive;
pub mod calendar;
pub mod cluster;
pub mod error;
pub mod feed;
pub mod fixture;
pub mod inspect;
pub mod limits;
#[cfg(feature = "network")]
pub mod network;
pub mod pack;
pub mod select;
pub mod table;
pub mod time;
pub mod walks;

pub use error::{Error, LimitKind, Result};
pub use feed::Feed;
pub use limits::Limits;
