//! Terminal-free adapter over the shared owner bootstrap and shutdown path.
use crate::{
    owner_session,
    protocol::{ApiError, ErrorCode, PROTOCOL_VERSION},
};
use serde_json::{Value, json};
use std::{io::Write, path::PathBuf, process::ExitCode};
use tokio_util::sync::CancellationToken;

pub fn startup_error(error: ApiError) -> ExitCode {
    let value = json!({"protocol_version":PROTOCOL_VERSION,"ok":false,"error":error});
    let _ = writeln!(std::io::stderr().lock(), "{value}");
    ExitCode::FAILURE
}
fn emit(value: &Value) -> bool {
    let mut output = std::io::stdout().lock();
    writeln!(output, "{value}")
        .and_then(|()| output.flush())
        .is_ok()
}
#[cfg(unix)]
async fn signal(signals: &mut crate::os_signals::ServeSignals) {
    let _ = signals.recv().await;
}
#[cfg(not(unix))]
async fn signal(_: &mut ()) {
    std::future::pending::<()>().await;
}
async fn listener_failure(
    failures: &mut tokio::sync::watch::Receiver<Option<ApiError>>,
) -> ApiError {
    loop {
        if let Some(error) = failures.borrow().clone() {
            return error;
        }
        if failures.changed().await.is_err() {
            return ApiError::new(ErrorCode::IoFailed, "Owner listener ended unexpectedly");
        }
    }
}

/// Err is startup-only; once ready, a failed final result is stdout plus exit 1.
pub async fn run(config: Option<PathBuf>, data: PathBuf) -> Result<bool, ApiError> {
    #[cfg(unix)]
    let mut signals = crate::os_signals::ServeSignals::new().map_err(|_| {
        ApiError::new(
            ErrorCode::IoFailed,
            "Cannot observe owner termination signals",
        )
    })?;
    #[cfg(not(unix))]
    let mut signals = ();
    let cancel = CancellationToken::new();
    let startup = owner_session::initialize_reporting(config, Some(data), cancel.clone(), |_| {});
    tokio::pin!(startup);
    let prepared = tokio::select! { biased;
        _ = signal(&mut signals) => { cancel.cancel(); startup.await },
        result = &mut startup => result,
    }?;
    let Some(prepared) = prepared else {
        return Ok(emit(
            &json!({"protocol_version":PROTOCOL_VERSION,"event":"stopped",
            "state":"cancelled","ok":true,"data_dir":null,"instance_id":null}),
        ));
    };
    if prepared.owner.loaded.data_dir.to_str().is_none() {
        prepared.shutdown().await?;
        return Err(ApiError::new(
            ErrorCode::InvalidArgument,
            "serve requires a UTF-8 data directory for its JSON readiness record",
        ));
    }
    let requested = prepared.control.owner_shutdown_signal();
    let mut failures = prepared.server.failures();
    let dispatcher = tokio::spawn(prepared.control.clone().serve(prepared.requests));
    let owner_session::Prepared {
        owner,
        control,
        server,
        ..
    } = prepared;
    let data = owner.loaded.data_dir.clone();
    let instance = owner.instance_id.clone();
    if !emit(
        &json!({"protocol_version":PROTOCOL_VERSION,"event":"ready","state":"ready",
        "data_dir":data,"instance_id":instance}),
    ) {
        let _ = owner_session::close(owner, Some(control), Some(server)).await;
        let _ = dispatcher.await;
        return Err(ApiError::new(
            ErrorCode::IoFailed,
            "Cannot write owner ready line",
        ));
    }
    let reason = tokio::select! { biased;
        _ = requested.cancelled() => Ok(()),
        _ = signal(&mut signals) => Ok(()),
        _ = control.stopping() => Ok(()),
        error = listener_failure(&mut failures) => Err(error),
    };
    control.freeze_owner_admission();
    let closed = owner_session::close(owner, Some(control), Some(server)).await;
    let dispatched = dispatcher
        .await
        .map_err(|_| ApiError::new(ErrorCode::InvalidState, "Owner dispatcher failed"));
    let result = reason.and(closed).and(dispatched);
    let ok = result.is_ok();
    let written = emit(
        &json!({"protocol_version":PROTOCOL_VERSION,"event":"stopped",
        "state":if ok {"stopped"} else {"failed"},"ok":ok,"data_dir":data,
        "instance_id":instance,"error":result.err()}),
    );
    Ok(ok && written)
}
