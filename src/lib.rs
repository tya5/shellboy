//! Shellboy native agent: typed operations, durable state and asynchronous workers.

pub mod activity;
pub mod app;
pub mod attachments;
pub mod cli;
pub mod compaction;
pub mod config;
pub mod config_setup;
pub mod context;
pub mod control;
pub mod deletion;
pub mod diagnostics;
mod digest;
pub mod docs;
pub mod exec_client;
pub mod frontend;
pub mod gc;
pub mod history;
pub mod index;
mod input_path;
pub mod instructions;
pub mod ipc;
mod json_pointer;
pub mod memory;
pub mod model;
pub mod presentation;
pub mod program_status;
pub mod protocol;
pub mod rag;
pub mod shell;
pub mod status;
pub mod storage;
pub mod terminal_links;
pub mod ui;

mod usage;

pub mod usage_totals;

pub(crate) mod readback;
#[cfg(test)]
#[path = "../tests/support/watchdog.rs"]
pub(crate) mod test_watchdog;
pub(crate) mod thread_observation;
