//! Bounded per-Thread saved facts and durable, instance-independent observations.
mod latest;
mod wait;
pub(crate) use latest::{
    RawLatest, last_end_message, last_input, prepare_latest_display, read_latest_display_tx,
};
pub(crate) use wait::{observe, validate};
