//! Integration suites share a harness; file-local fixtures and cfgs remain isolated.
#[path = "../app_ipc.rs"]
mod app_ipc;
#[path = "../attachment_context.rs"]
mod attachment_context;
#[path = "../attachment_delivery.rs"]
mod attachment_delivery;
#[path = "../attachment_provenance.rs"]
mod attachment_provenance;
#[path = "../attachment_proxy.rs"]
mod attachment_proxy;
#[path = "../attachments.rs"]
mod attachments;
#[path = "../chat_completions.rs"]
mod chat_completions;
#[path = "../cli.rs"]
mod cli;
#[path = "../cli_process.rs"]
mod cli_process;
#[path = "../composer.rs"]
mod composer;
#[path = "../config.rs"]
mod config;
#[path = "../config_setup.rs"]
mod config_setup;
#[path = "../context.rs"]
mod context;
#[path = "../control_db.rs"]
mod control_db;
#[path = "../deletion.rs"]
mod deletion;
#[path = "../dependency_smoke.rs"]
mod dependency_smoke;
#[path = "../diagnostics.rs"]
mod diagnostics;
#[path = "../docs_cli.rs"]
mod docs_cli;
#[path = "../docs_proxy.rs"]
mod docs_proxy;
#[path = "../frontend.rs"]
mod frontend;
#[path = "../frontend_activity.rs"]
mod frontend_activity;
#[path = "../frontend_ordered.rs"]
mod frontend_ordered;
#[path = "../frontend_reactivity.rs"]
mod frontend_reactivity;
#[path = "../gc.rs"]
mod gc;
#[path = "../history.rs"]
mod history;
#[path = "../history_append.rs"]
mod history_append;
#[path = "../history_relations.rs"]
mod history_relations;
#[path = "../http_deadlines.rs"]
mod http_deadlines;
#[path = "../http_header_measurement.rs"]
mod http_header_measurement;
#[path = "../http_observation.rs"]
mod http_observation;
#[path = "../index.rs"]
mod index;
#[path = "../index_batch.rs"]
mod index_batch;
#[path = "../index_dimensions.rs"]
mod index_dimensions;
#[path = "../input_history_backend.rs"]
mod input_history_backend;
#[path = "../instructions.rs"]
mod instructions;
#[path = "../message_recovery.rs"]
mod message_recovery;
#[path = "../model.rs"]
mod model;
#[path = "../model_catalog.rs"]
mod model_catalog;
#[path = "../model_http.rs"]
mod model_http;
#[path = "../model_list.rs"]
mod model_list;
#[path = "../model_picker.rs"]
mod model_picker;
#[path = "../model_tls.rs"]
mod model_tls;
#[path = "../operation_contract.rs"]
mod operation_contract;
#[path = "../output.rs"]
mod output;
#[path = "../performance.rs"]
mod performance;
#[path = "../performance_lifecycle.rs"]
mod performance_lifecycle;
#[path = "../performance_load.rs"]
mod performance_load;
#[path = "../performance_tui.rs"]
mod performance_tui;
#[path = "../presentation.rs"]
mod presentation;
#[path = "../program_status.rs"]
mod program_status;
#[path = "../protocol.rs"]
mod protocol;
#[path = "../query_index_performance.rs"]
mod query_index_performance;
#[path = "../rag_cli.rs"]
mod rag_cli;
#[path = "../rag_extract.rs"]
mod rag_extract;
#[path = "../rag_search_readiness.rs"]
mod rag_search_readiness;
#[path = "../rag_selection_status.rs"]
mod rag_selection_status;
#[path = "../rag_store.rs"]
mod rag_store;
#[path = "../rag_thread_scope.rs"]
mod rag_thread_scope;
#[path = "../recovery_paths.rs"]
mod recovery_paths;
#[path = "../replay_cache.rs"]
mod replay_cache;
#[path = "../response_retries.rs"]
mod response_retries;
#[path = "../response_upload_recovery.rs"]
mod response_upload_recovery;
#[path = "../revisit.rs"]
mod revisit;
#[path = "../runtime.rs"]
mod runtime;
#[path = "../runtime_features.rs"]
mod runtime_features;
#[path = "../runtime_hierarchy.rs"]
mod runtime_hierarchy;
#[path = "../scrolling.rs"]
mod scrolling;
#[path = "../shell.rs"]
mod shell;
#[path = "../shell_windows.rs"]
mod shell_windows;
#[path = "../shell_windows_command.rs"]
mod shell_windows_command;
#[path = "../storage.rs"]
mod storage;
#[path = "../storage_checkpoint.rs"]
mod storage_checkpoint;
#[path = "../storage_hot_queries.rs"]
mod storage_hot_queries;
#[path = "../terminal_links.rs"]
mod terminal_links;
#[path = "../thread_browser.rs"]
mod thread_browser;
#[path = "../thread_exit_marker.rs"]
mod thread_exit_marker;
#[path = "../thread_titles.rs"]
mod thread_titles;
#[path = "../tool_cancel_reasons.rs"]
mod tool_cancel_reasons;
#[path = "../tool_group_attachment_recovery.rs"]
mod tool_group_attachment_recovery;
#[path = "../tool_repair_isolation.rs"]
mod tool_repair_isolation;
#[path = "../tui_process.rs"]
mod tui_process;
#[path = "../tui_recovery.rs"]
mod tui_recovery;
#[path = "../tui_shutdown.rs"]
mod tui_shutdown;
#[path = "../turn_interrupt.rs"]
mod turn_interrupt;
#[path = "../ui.rs"]
mod ui;
#[path = "../ui_scroll_refresh.rs"]
mod ui_scroll_refresh;
#[path = "../ui_scroll_stream.rs"]
mod ui_scroll_stream;
#[path = "../ui_thread_deletion.rs"]
mod ui_thread_deletion;
#[path = "../work_status.rs"]
mod work_status;

#[path = "../index_space_gc.rs"]
mod index_space_gc;

#[path = "../exec_client.rs"]
mod exec_client;
