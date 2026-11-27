//! Bounded per-Thread saved facts and durable, instance-independent observations.
mod latest;
mod wait;
pub(crate) use latest::{last_end_message, last_input};
pub(crate) use wait::{observe, validate};
