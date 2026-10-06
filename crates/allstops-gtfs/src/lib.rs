//! GTFS loading for allstops: input limits, service calendars, station
//! clustering, target selection and footpaths. No filesystem or network
//! access, so it compiles unchanged to WebAssembly; callers pass bytes in.

pub mod archive;
pub mod calendar;
pub mod cluster;
pub mod error;
pub mod feed;
pub mod fixture;
pub mod inspect;
pub mod limits;
pub mod network;
pub mod select;
pub mod table;
pub mod time;

pub use error::{Error, LimitKind, Result};
pub use feed::Feed;
pub use limits::Limits;
