//! Configuration schema and settings registry.

pub mod schema;
pub mod settings;

pub use schema::*;
pub use settings::{Section, SettingDef, SettingKind};
