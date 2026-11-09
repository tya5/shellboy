//! Shellboy native agent: typed operations, durable state and asynchronous workers.

pub mod activity;
pub mod app;
pub mod attachments;
pub mod chat_client;
pub mod cli;
pub mod client_operation;
pub mod compaction;
pub mod config;
pub mod config_edit;
pub mod config_setup;
pub mod context;
pub mod control;
pub mod credential_store;
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
pub mod maintenance;
pub mod memory;
pub mod model;
mod operations;
#[cfg(unix)]
mod os_signals;
pub mod owner_session;
pub mod presentation;
pub mod program_status;
pub mod protocol;
pub mod rag;
pub mod request_inspect;
pub mod serve;
pub mod shell;
pub mod startup;
pub mod status;
pub mod storage;
pub mod terminal_links;
pub(crate) mod tool_description;
#[doc(hidden)]
pub mod trace_slot;
pub mod ui;

mod usage;

pub mod usage_totals;

pub(crate) mod readback;
#[cfg(test)]
#[path = "../tests/support/watchdog.rs"]
pub(crate) mod test_watchdog;
pub(crate) mod thread_observation;

#[cfg(test)]
mod shared_ui_message_tests {
    use crate::{config::UiLanguage, ui::i18n::local};

    #[test]
    fn fixed_messages_are_available_outside_ui_and_evaluate_language_once() {
        assert_eq!(
            local!(UiLanguage::Ja, "\u{65e5}\u{672c}\u{8a9e}", "English"),
            "\u{65e5}\u{672c}\u{8a9e}"
        );
        assert_eq!(
            local!(UiLanguage::En, "\u{65e5}\u{672c}\u{8a9e}", "English"),
            "English"
        );
        let mut evaluations = 0;
        let text = local!(
            {
                evaluations += 1;
                UiLanguage::En
            },
            "\u{65e5}\u{672c}\u{8a9e}",
            "English"
        );
        assert_eq!(text, "English");
        assert_eq!(evaluations, 1);
    }
}

pub mod width_cache;

pub(crate) mod stdin_facts;

#[cfg(test)]
mod stdin_compat;
