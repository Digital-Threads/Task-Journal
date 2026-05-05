//! tj-core: append-only event log + derived SQLite state for Task Journal.

#![deny(rust_2018_idioms)]

pub mod classifier;
pub mod db;
pub mod event;
pub mod pack;
pub mod paths;
pub mod project_hash;
pub mod session;
pub mod storage;
