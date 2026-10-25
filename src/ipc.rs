//! One-owner local IPC. Observer cancellation never cancels accepted mutations.
use crate::{
    app::Endpoint,
    protocol::{
        ApiError, Command, ErrorCode, Origin, PROTOCOL_VERSION, Request, Response, encode_frame,
        read_frame, write_encoded_frame, write_frame,
    },
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::Path;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite},
    sync::{OwnedSemaphorePermit, mpsc, oneshot},
};
use tokio_util::sync::CancellationToken;

mod server;
pub use server::LocalServer;

const FRAME_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Hello {
    protocol_version: u32,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Welcome {
    protocol_version: u32,
    instance_id: String,
}
pub struct Invocation {
    pub request: Request,
    pub reply: oneshot::Sender<Response<Value>>,
    /// Only wait/observation should consult this; accepted writes must continue.
    pub disconnected: CancellationToken,
    /// Retained by the consumer even after the transport observer disconnects.
    pub(crate) admission: OwnedSemaphorePermit,
}
fn io_error(_: std::io::Error) -> ApiError {
    ApiError::new(
        ErrorCode::AppNotRunning,
        "Cannot connect to the owner of this data directory",
    )
    .hint("Check --data-dir and start its owner if needed; inspect any known operation receipt before repeating a mutation")
}
fn mismatch() -> ApiError {
    ApiError::new(
        ErrorCode::ProtocolMismatch,
        "Local endpoint or protocol version changed; retry the connection",
    )
}

async fn serve_connection<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    instance: String,
    requests: mpsc::Sender<Invocation>,
    stop: CancellationToken,
    work: server::WorkAdmission,
) -> Result<(), ApiError> {
    let disconnected = CancellationToken::new();
    let operation = async {
        let hello: Hello = tokio::time::timeout(FRAME_READ_TIMEOUT, read_frame(&mut stream))
            .await
            .map_err(|_| mismatch())??
            .ok_or_else(mismatch)?;
        if hello.protocol_version != PROTOCOL_VERSION {
            return Err(mismatch());
        }
        write_frame(
            &mut stream,
            &Welcome {
                protocol_version: PROTOCOL_VERSION,
                instance_id: instance,
            },
        )
        .await?;
        let request: Request = tokio::time::timeout(FRAME_READ_TIMEOUT, read_frame(&mut stream))
            .await
            .map_err(|_| ApiError::new(ErrorCode::IoFailed, "Request frame read timed out"))??
            .ok_or_else(mismatch)?;
        if let Err(error) = request.validate() {
            write_frame(
                &mut stream,
                &Response::<Value>::failure(request.operation_id, error),
            )
            .await?;
            return Ok(());
        }
        let admission = match work.try_acquire(&request.command) {
            Ok(permit) => permit,
            Err(error) => {
                write_frame(
                    &mut stream,
                    &Response::<Value>::failure(request.operation_id, error),
                )
                .await?;
                return Ok(());
            }
        };
        let (reply, receiver) = oneshot::channel();
        requests
            .send(Invocation {
                request,
                reply,
                disconnected: disconnected.clone(),
                admission,
            })
            .await
            .map_err(|_| ApiError::new(ErrorCode::AppNotRunning, "Owner is shutting down"))?;
        let mut byte = [0u8; 1];
        tokio::select! {
            result=receiver=>{
                let response=result.map_err(|_|ApiError::new(ErrorCode::IoFailed,"Operation reply unavailable"))?;
                write_frame(&mut stream,&response).await?;
                Ok(())
            },
            _=stream.read(&mut byte)=>Ok(()),
        }
    };
    let result = tokio::select! {result=operation=>result,_=stop.cancelled()=>Ok(())};
    disconnected.cancel();
    result
}

pub async fn call(
    data_dir: &Path,
    command: Command,
    operation_id: Option<String>,
    origin: Option<Origin>,
) -> Result<Response<Value>, ApiError> {
    let path = data_dir.join("endpoint.json");
    let endpoint = tokio::task::spawn_blocking(move || {
        let file = std::fs::File::open(path).map_err(io_error)?;
        use std::io::Read;
        let mut bytes = Vec::new();
        file.take(64 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(io_error)?;
        if bytes.len() > 64 * 1024 {
            return Err(mismatch());
        }
        serde_json::from_slice::<Endpoint>(&bytes).map_err(|_| mismatch())
    })
    .await
    .map_err(|_| ApiError::new(ErrorCode::IoFailed, "Endpoint worker failed"))??;
    if endpoint.protocol_version != PROTOCOL_VERSION {
        return Err(mismatch());
    }
    #[cfg(unix)]
    {
        if endpoint.transport != "unix" {
            return Err(mismatch());
        }
        let stream = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            tokio::net::UnixStream::connect(&endpoint.address),
        )
        .await
        .map_err(|_| io_error(std::io::ErrorKind::TimedOut.into()))?
        .map_err(io_error)?;
        call_stream(stream, endpoint, command, operation_id, origin).await
    }
    #[cfg(windows)]
    {
        if endpoint.transport != "pipe" {
            return Err(mismatch());
        }
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
        let stream = loop {
            match tokio::net::windows::named_pipe::ClientOptions::new().open(&endpoint.address) {
                Ok(client) => break client,
                Err(error)
                    if error.raw_os_error() == Some(231)
                        && tokio::time::Instant::now() < deadline =>
                {
                    tokio::time::sleep(std::time::Duration::from_millis(10)).await
                }
                Err(error) => return Err(io_error(error)),
            }
        };
        call_stream(stream, endpoint, command, operation_id, origin).await
    }
}
fn uncertain_reply(mut error: ApiError, operation_id: &str, command: &Command) -> ApiError {
    if !command.requires_receipt() {
        error.hint=Some("Repeat shellboy status with the same filters to observe fresh state; this read-only projection does not create an operation receipt".into());
        error.details = Some(
            serde_json::json!({"operation_id":operation_id,"read_only":true,"request_may_have_been_applied":false}),
        );
        return error;
    }
    if command.is_read_only() {
        error.hint = Some(format!(
            "The {} observation does not change target state, but its receipt or result may have been saved. Use details.operation_id with shellboy operation get --operation OPERATION_ID or replay the same operation ID for the original result. Use a fresh operation ID only for a new observation. An older owner may not support this command; a missing receipt does not prove a previous mutation was never applied. Check CLI/owner release compatibility, and do not resubmit an unconfirmed mutation under a new ID",
            command.name()
        ));
        error.details = Some(serde_json::json!({
            "operation_id": operation_id,
            "read_only": true,
            "request_may_have_been_applied": true,
            "next_operation": Command::OperationGet(crate::protocol::OperationTarget {operation_id:operation_id.into()})
        }));
        return error;
    }
    error.hint = Some("Use details.operation_id with shellboy operation get --operation OPERATION_ID, then inspect saved target state. An older owner may not support this command; a missing receipt does not prove the request was never applied. If receipt lookup is also unsupported, inspect the target using commands available on that owner. Check that CLI and owner use the same release; do not resubmit the mutation under a new ID".into());
    error.details = Some(serde_json::json!({
        "operation_id": operation_id,
        "request_may_have_been_applied": true,
        "next_operation": Command::OperationGet(crate::protocol::OperationTarget {operation_id:operation_id.into()})
    }));
    error
}

fn handshake_timeout() -> ApiError {
    ApiError::new(ErrorCode::IoFailed,"Local IPC handshake timed out; the request was not sent")
        .hint("Check that the owner is responsive and retry the connection; do not infer a protocol version change from this timeout")
        .details(serde_json::json!({"phase":"handshake","request_may_have_been_applied":false}))
}
async fn call_stream<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    endpoint: Endpoint,
    command: Command,
    operation_id: Option<String>,
    origin: Option<Origin>,
) -> Result<Response<Value>, ApiError> {
    write_frame(
        &mut stream,
        &Hello {
            protocol_version: PROTOCOL_VERSION,
        },
    )
    .await?;
    let welcome: Welcome =
        tokio::time::timeout(std::time::Duration::from_secs(5), read_frame(&mut stream))
            .await
            .map_err(|_| handshake_timeout())??
            .ok_or_else(mismatch)?;
    if welcome.protocol_version != PROTOCOL_VERSION || welcome.instance_id != endpoint.instance_id {
        return Err(mismatch());
    }
    let mut request = Request::new(
        operation_id.unwrap_or_else(|| format!("{}:{}", welcome.instance_id, uuid::Uuid::new_v4())),
        command,
    );
    request.origin = origin;
    request.validate()?;
    let expected = request.operation_id.clone();
    let encoded = encode_frame(&request).map_err(|mut error| {
        error.hint = Some("The request was not sent. Reduce the input so the complete JSON request fits the frame limit, then retry".into());
        error.details = Some(serde_json::json!({
            "operation_id": expected,
            "request_may_have_been_applied": false
        }));
        error
    })?;
    write_encoded_frame(&mut stream, &encoded)
        .await
        .map_err(|error| uncertain_reply(error, &expected, &request.command))?;
    let response: Response<Value> = read_frame(&mut stream)
        .await
        .map_err(|error| uncertain_reply(error, &expected, &request.command))?
        .ok_or_else(|| {
            ApiError::new(
                ErrorCode::IoFailed,
                "Connection ended before reply; the operation result is unconfirmed",
            )
        })
        .map_err(|error| uncertain_reply(error, &expected, &request.command))?;
    response
        .validate()
        .map_err(|error| uncertain_reply(error, &expected, &request.command))?;
    if response.operation_id != expected {
        return Err(uncertain_reply(mismatch(), &expected, &request.command));
    }
    Ok(response)
}

#[cfg(test)]
mod frame_delivery_tests {
    use super::*;
    use crate::protocol::{MAX_FRAME_BYTES, MessageSendArgs};

    fn endpoint() -> Endpoint {
        Endpoint {
            protocol_version: PROTOCOL_VERSION,
            instance_id: "instance".into(),
            transport: "fixture".into(),
            address: "fixture".into(),
        }
    }

    #[tokio::test]
    async fn handshake_timeout_is_unsent_io_failure_instead_of_protocol_mismatch() {
        let (client, mut peer) = tokio::io::duplex(4096);
        let server = tokio::spawn(async move {
            read_frame::<_, Hello>(&mut peer).await.unwrap().unwrap();
            let mut byte = [0u8; 1];
            assert_eq!(
                peer.read(&mut byte).await.unwrap(),
                0,
                "no request may be sent without a welcome"
            );
        });
        let error = call_stream(
            client,
            endpoint(),
            Command::Status(Default::default()),
            None,
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::IoFailed);
        let details = error.details.unwrap();
        assert_eq!(details["phase"], "handshake");
        assert_eq!(details["request_may_have_been_applied"], false);
        server.await.unwrap();
    }
    #[tokio::test]
    async fn envelope_overflow_is_unsent_and_exact_frame_limit_is_accepted() {
        let operation = "instance:oversized";
        let make = |text| {
            Command::MessageSend(MessageSendArgs {
                thread_id: Some("t".into()),
                text,
            })
        };
        let overhead = serde_json::to_vec(&Request::new(operation, make(String::new())))
            .unwrap()
            .len();
        let text = "x".repeat(MAX_FRAME_BYTES - overhead);
        let exact = Request::new(operation, make(text.clone()));
        assert_eq!(serde_json::to_vec(&exact).unwrap().len(), MAX_FRAME_BYTES);
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &exact).await.unwrap();
        assert_eq!(bytes.len(), MAX_FRAME_BYTES + 4);
        assert_eq!(
            read_frame::<_, Request>(&mut bytes.as_slice())
                .await
                .unwrap()
                .unwrap(),
            exact
        );

        let (client, mut peer) = tokio::io::duplex(4096);
        let task = tokio::spawn(async move {
            read_frame::<_, Hello>(&mut peer).await.unwrap().unwrap();
            write_frame(
                &mut peer,
                &Welcome {
                    protocol_version: PROTOCOL_VERSION,
                    instance_id: "instance".into(),
                },
            )
            .await
            .unwrap();
            // Preflight refusal must close without sending even a request prefix.
            assert!(read_frame::<_, Request>(&mut peer).await.unwrap().is_none());
        });
        let error = call_stream(
            client,
            endpoint(),
            make(text + "x"),
            Some(operation.into()),
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::InputLimit);
        let details = error.details.unwrap();
        assert_eq!(details["operation_id"], operation);
        assert_eq!(details["request_may_have_been_applied"], false);
        assert!(details.get("next_operation").is_none());
        task.await.unwrap();
    }

    #[tokio::test]
    async fn lost_reply_after_sent_request_keeps_the_operation_uncertain() {
        let (client, mut peer) = tokio::io::duplex(4096);
        let task = tokio::spawn(async move {
            read_frame::<_, Hello>(&mut peer).await.unwrap().unwrap();
            write_frame(
                &mut peer,
                &Welcome {
                    protocol_version: PROTOCOL_VERSION,
                    instance_id: "instance".into(),
                },
            )
            .await
            .unwrap();
            let request = read_frame::<_, Request>(&mut peer).await.unwrap().unwrap();
            assert_eq!(request.operation_id, "instance:sent");
            // A received request is no proof that its reply/receipt reached this client.
        });
        let error = call_stream(
            client,
            endpoint(),
            Command::MessageSend(MessageSendArgs {
                thread_id: Some("t".into()),
                text: "hello".into(),
            }),
            Some("instance:sent".into()),
            None,
        )
        .await
        .unwrap_err();
        let details = error.details.unwrap();
        assert_eq!(details["operation_id"], "instance:sent");
        assert_eq!(details["request_may_have_been_applied"], true);
        assert_eq!(details["next_operation"]["command"], "operation.get");
        task.await.unwrap();
    }

    #[tokio::test]
    async fn lost_reply_distinguishes_receipt_free_reads_saved_reads_and_stateful_waits() {
        let cases = [
            Command::Status(Default::default()),
            Command::ThreadGet(crate::protocol::ThreadTarget {
                thread_id: Some("t".into()),
            }),
            Command::IndexStatus(crate::protocol::IndexArgs {
                scope: None,
                space_id: None,
                thread_ids: Vec::new(),
                source_ids: Vec::new(),
            }),
            Command::Wait(crate::protocol::WaitArgs {
                conditions: vec![crate::protocol::WaitCondition::RunEnded { run_id: "r".into() }],
                timeout_ms: 0,
            }),
        ];
        for command in cases {
            let name = command.name();
            let (client, mut peer) = tokio::io::duplex(4096);
            let task = tokio::spawn(async move {
                read_frame::<_, Hello>(&mut peer).await.unwrap().unwrap();
                write_frame(
                    &mut peer,
                    &Welcome {
                        protocol_version: PROTOCOL_VERSION,
                        instance_id: "instance".into(),
                    },
                )
                .await
                .unwrap();
                let request = read_frame::<_, Request>(&mut peer).await.unwrap().unwrap();
                assert_eq!(request.command.name(), name);
                // Drop exactly after receiving the request, without dispatching
                // any fixture mutation or assuming a receipt reached the client.
            });
            let error = call_stream(
                client,
                endpoint(),
                command,
                Some("instance:lost".into()),
                None,
            )
            .await
            .unwrap_err();
            let hint = error.hint.unwrap();
            let details = error.details.unwrap();
            assert_eq!(details["operation_id"], "instance:lost");
            match name {
                "status" => {
                    assert_eq!(details["read_only"], true);
                    assert_eq!(details["request_may_have_been_applied"], false);
                    assert!(details.get("next_operation").is_none());
                    assert!(hint.contains("does not create an operation receipt"));
                }
                "wait" => {
                    assert_eq!(details["request_may_have_been_applied"], true);
                    assert_ne!(details["read_only"], true);
                    assert!(hint.contains("do not resubmit the mutation under a new ID"));
                }
                _ => {
                    assert_eq!(details["read_only"], true);
                    assert_eq!(details["request_may_have_been_applied"], true);
                    assert_eq!(details["next_operation"]["command"], "operation.get");
                    assert!(hint.contains(name));
                    assert!(hint.contains("does not change target state"));
                    assert!(hint.contains("replay the same operation ID"));
                    assert!(hint.contains("fresh operation ID only for a new observation"));
                }
            }
            task.await.unwrap();
        }
    }

    #[tokio::test]
    async fn older_v1_command_decoder_disconnect_is_uncertain_and_explains_compatibility() {
        // The pre-extension command set does not know operation.get, while its
        // v1 envelope and hello/welcome are identical to the current ones.
        #[derive(Deserialize)]
        #[serde(tag = "command", content = "args")]
        enum OldCommand {
            #[serde(rename = "thread.list")]
            ThreadList(crate::protocol::ThreadListArgs),
        }
        let known = serde_json::from_value::<OldCommand>(
            serde_json::json!({"command":"thread.list","args":{"limit":1}}),
        )
        .unwrap();
        let OldCommand::ThreadList(args) = known;
        assert_eq!(args.limit, 1);
        let (client, mut peer) = tokio::io::duplex(4096);
        let task = tokio::spawn(async move {
            read_frame::<_, Hello>(&mut peer).await.unwrap().unwrap();
            write_frame(
                &mut peer,
                &Welcome {
                    protocol_version: PROTOCOL_VERSION,
                    instance_id: "instance".into(),
                },
            )
            .await
            .unwrap();
            let request = read_frame::<_, Value>(&mut peer).await.unwrap().unwrap();
            assert_eq!(request["protocol_version"], 1);
            assert_eq!(request["operation_id"], "instance:unsupported");
            let error = serde_json::from_value::<OldCommand>(request).err().unwrap();
            assert!(error.to_string().contains("unknown variant"));
            // Like the old server, close on decode failure before dispatch.
        });
        let error = call_stream(
            client,
            endpoint(),
            Command::OperationGet(crate::protocol::OperationTarget {
                operation_id: "instance:previous".into(),
            }),
            Some("instance:unsupported".into()),
            None,
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::IoFailed);
        let hint = error.hint.unwrap();
        assert!(hint.contains("older owner"));
        assert!(hint.contains("missing receipt does not prove"));
        assert!(hint.contains("do not resubmit"));
        let details = error.details.unwrap();
        assert_eq!(details["operation_id"], "instance:unsupported");
        assert_eq!(details["request_may_have_been_applied"], true);
        task.await.unwrap();
    }

    #[tokio::test]
    async fn malformed_request_frames_close_without_dispatch_or_a_fabricated_reply() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for bytes in [
            vec![0, 0, 0, 1, b'{'],
            vec![0, 0, 0, 0],
            vec![0, 0, 0, 8, b'{'],
            ((MAX_FRAME_BYTES + 1) as u32).to_be_bytes().to_vec(),
        ] {
            let (mut client, server) = tokio::io::duplex(4096);
            let (requests, mut incoming) = mpsc::channel(1);
            let task = tokio::spawn(serve_connection(
                server,
                "instance".into(),
                requests,
                CancellationToken::new(),
                server::WorkAdmission::new(),
            ));
            write_frame(
                &mut client,
                &Hello {
                    protocol_version: PROTOCOL_VERSION,
                },
            )
            .await
            .unwrap();
            read_frame::<_, Welcome>(&mut client)
                .await
                .unwrap()
                .unwrap();
            client.write_all(&bytes).await.unwrap();
            client.shutdown().await.unwrap();
            assert!(task.await.unwrap().is_err());
            assert!(incoming.recv().await.is_none());
            let mut response = Vec::new();
            client.read_to_end(&mut response).await.unwrap();
            assert!(response.is_empty());
        }
    }
    #[tokio::test]
    async fn decoded_invalid_request_gets_its_own_id_and_closes_without_dispatch() {
        let (mut client, server) = tokio::io::duplex(4096);
        let (requests, mut incoming) = mpsc::channel(1);
        let task = tokio::spawn(serve_connection(
            server,
            "instance".into(),
            requests,
            CancellationToken::new(),
            server::WorkAdmission::new(),
        ));
        write_frame(
            &mut client,
            &Hello {
                protocol_version: PROTOCOL_VERSION,
            },
        )
        .await
        .unwrap();
        read_frame::<_, Welcome>(&mut client)
            .await
            .unwrap()
            .unwrap();
        let command = Command::ThreadList(crate::protocol::ThreadListArgs {
            limit: 0,
            ..Default::default()
        });
        write_frame(&mut client, &Request::new("instance:invalid", command))
            .await
            .unwrap();
        let response: Response<Value> = read_frame(&mut client).await.unwrap().unwrap();
        response.validate().unwrap();
        assert_eq!(response.operation_id, "instance:invalid");
        assert_eq!(response.error.unwrap().code, ErrorCode::InvalidArgument);
        task.await.unwrap().unwrap();
        assert!(incoming.recv().await.is_none());
        assert!(read_frame::<_, Value>(&mut client).await.unwrap().is_none());
    }
    #[tokio::test]
    async fn partial_request_prefix_and_payload_time_out_without_dispatch() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let partials = [
            vec![0, 0],
            [(MAX_FRAME_BYTES as u32).to_be_bytes().as_slice(), b"{"].concat(),
        ];
        let mut cases = tokio::task::JoinSet::new();
        for bytes in partials {
            cases.spawn(async move {
                let (mut client, server) = tokio::io::duplex(4096);
                let (requests, mut incoming) = mpsc::channel(1);
                let task = tokio::spawn(serve_connection(
                    server,
                    "instance".into(),
                    requests,
                    CancellationToken::new(),
                    server::WorkAdmission::new(),
                ));
                write_frame(
                    &mut client,
                    &Hello {
                        protocol_version: PROTOCOL_VERSION,
                    },
                )
                .await
                .unwrap();
                read_frame::<_, Welcome>(&mut client)
                    .await
                    .unwrap()
                    .unwrap();
                client.write_all(&bytes).await.unwrap();
                let error = tokio::time::timeout(
                    FRAME_READ_TIMEOUT + std::time::Duration::from_secs(1),
                    task,
                )
                .await
                .unwrap()
                .unwrap()
                .unwrap_err();
                assert_eq!(error.code, ErrorCode::IoFailed);
                assert!(error.message.contains("timed out"));
                assert!(incoming.recv().await.is_none());
                let mut response = Vec::new();
                client.read_to_end(&mut response).await.unwrap();
                assert!(response.is_empty());
            });
        }
        while let Some(result) = cases.join_next().await {
            result.unwrap();
        }
    }
}
