//! Terminal-free adapter over the shared owner bootstrap.
use crate::{
    owner_session,
    protocol::{ApiError, ErrorCode, PROTOCOL_VERSION},
};
use serde_json::json;
use std::{io::Write, path::PathBuf, process::ExitCode};
use tokio_util::sync::CancellationToken;

pub fn startup_error(error: ApiError) -> ExitCode {
    let value = json!({"protocol_version":PROTOCOL_VERSION,"ok":false,"error":error});
    let _ = writeln!(std::io::stderr().lock(), "{value}");
    ExitCode::FAILURE
}

pub async fn run(config: Option<PathBuf>, data: PathBuf) -> Result<(), ApiError> {
    let prepared =
        owner_session::initialize_reporting(config, Some(data), CancellationToken::new(), |_| {})
            .await?
            .ok_or_else(|| ApiError::new(ErrorCode::InvalidState, "Owner startup cancelled"))?;
    let dispatcher = tokio::spawn(prepared.control.clone().serve(prepared.requests));
    // Retain cleanup ownership after handing the receiver to the dispatcher.
    let owner_session::Prepared {
        owner,
        control,
        server,
        ..
    } = prepared;
    let ready = json!({"protocol_version":PROTOCOL_VERSION,"event":"ready","state":"ready",
        "data_dir":owner.loaded.data_dir,"instance_id":owner.instance_id});
    let written = {
        let mut output = std::io::stdout().lock();
        writeln!(output, "{ready}").and_then(|()| output.flush())
    };
    if written.is_err() {
        let _ = owner_session::close(owner, Some(control), Some(server)).await;
        let _ = dispatcher.await;
        return Err(ApiError::new(
            ErrorCode::IoFailed,
            "Cannot write owner ready line",
        ));
    }
    let _session = (owner, control, server, dispatcher);
    // Stage 3 will provide the explicit owner shutdown trigger; stdin is never one.
    std::future::pending::<()>().await;
    unreachable!()
}
