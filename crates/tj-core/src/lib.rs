//! tj-core: append-only event log + derived SQLite state for Task Journal.

#![deny(rust_2018_idioms)]

pub mod event;
pub mod storage;
pub mod paths;
pub mod project_hash;
pub mod db;
pub mod pack;
pub mod classifier;
