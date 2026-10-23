//! Core building blocks for Caffeine: the shared error model, durations,
//! time helpers, file locations, configuration, power-management types and
//! the platform-independent session engine.
//!
//! This crate contains no operating-system specific power-management code;
//! native backends live in `caffeine-platform` and implement the
//! [`power::PowerBackend`] trait defined here.

pub mod config;
pub mod duration;
pub mod error;
pub mod fsutil;
pub mod paths;
pub mod power;
pub mod time;

pub use error::{Error, ErrorKind, Result};
pub use paths::Paths;

/// Version of this build of Caffeine.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
