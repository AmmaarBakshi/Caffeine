//! Core building blocks for Caffeine: the shared error model, durations,
//! time helpers, file locations, configuration, power-management types and
//! the platform-independent session engine.
//!
//! This crate contains no operating-system specific power-management code;
//! native backends live in `caffeine-platform` and implement the
//! [`power::PowerBackend`] trait defined here.


pub mod error;

pub use error::{Error, ErrorKind, Result};

/// Version of this build of Caffeine.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
