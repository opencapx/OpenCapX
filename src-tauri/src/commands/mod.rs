//! Tauri command modules, grouped by domain. Mechanical move from main.rs.

// Path-stability layer: the glob re-exports keep every crate::<...>::X path working
// after the module splits; clippy flags the ones nothing outside consumes yet.
#![allow(unused_imports)]
use super::*;
pub(crate) mod alerting;
pub(crate) mod lifecycle;
pub(crate) mod marketplace;
pub(crate) mod observability;
pub(crate) mod permissions;
pub(crate) mod petpacks;
pub(crate) mod plugins;
pub(crate) mod sessions;

pub(crate) use alerting::*;
pub(crate) use lifecycle::*;
pub(crate) use marketplace::*;
pub(crate) use observability::*;
pub(crate) use permissions::*;
pub(crate) use petpacks::*;
pub(crate) use plugins::*;
pub(crate) use sessions::*;
