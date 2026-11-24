//! Per-request transport deadlines, shared by headers and every body reader.
use super::*;
use std::{future::Future, time::Duration};
use tokio::time::Instant;

const MODEL_CATALOG_TIMEOUT_MS: u64 = 20_000;
#[derive(Clone, Copy)]
enum DeadlineKind {
    Total {
        limit: Duration,
        endpoint: ApiEndpoint,
    },
    Inactivity(Duration),
    Disabled,
}
impl DeadlineKind {
    fn limit(&self) -> Duration {
        match self {
            Self::Total { limit, .. } | Self::Inactivity(limit) => *limit,
            Self::Disabled => Duration::ZERO,
        }
    }
}
pub(super) struct Deadline {
    started: Instant,
    received: Instant,
    kind: DeadlineKind,
    setting: String,
    header: Option<(Duration, String)>,
}
impl Deadline {
    pub(super) fn new(
        endpoint: ApiEndpoint,
        connection: &Connection,
        name: &str,
    ) -> Result<Self, ModelError> {
        let (kind, key) = match endpoint {
            ApiEndpoint::Responses => {
                let milliseconds = connection.stream_idle_timeout_ms;
                (
                    if milliseconds == 0 {
                        DeadlineKind::Disabled
                    } else {
                        DeadlineKind::Inactivity(Duration::from_millis(milliseconds))
                    },
                    "stream_idle_timeout_ms",
                )
            }
            _ => {
                let milliseconds = if matches!(endpoint, ApiEndpoint::Models) {
                    connection.request_timeout_ms.min(MODEL_CATALOG_TIMEOUT_MS)
                } else {
                    connection.request_timeout_ms
                };
                (
                    DeadlineKind::Total {
                        limit: Duration::from_millis(milliseconds),
                        endpoint,
                    },
                    "request_timeout_ms",
                )
            }
        };
        let now = Instant::now();
        let setting = format!("connections.{name}.{key}");
        if now.checked_add(kind.limit()).is_none() {
            return Err(ModelError::new(
                "config_invalid",
                format!("{setting} exceeds the monotonic clock range"),
            ));
        }
        let header = matches!(endpoint, ApiEndpoint::Responses).then(|| {
            (
                Duration::from_millis(connection.response_header_timeout_ms),
                format!("connections.{name}.response_header_timeout_ms"),
            )
        });
        if header
            .as_ref()
            .is_some_and(|(limit, _)| now.checked_add(*limit).is_none())
        {
            return Err(ModelError::new(
                "config_invalid",
                "Response header deadline exceeds the monotonic clock range",
            ));
        }
        Ok(Self {
            started: now,
            received: now,
            kind,
            setting,
            header,
        })
    }
    pub(super) async fn wait_headers<F: Future>(
        &self,
        cancel: &CancellationToken,
        future: F,
    ) -> Result<F::Output, ModelError> {
        let Some((limit, setting)) = &self.header else {
            return self
                .wait(cancel, "waiting for response headers", future)
                .await;
        };
        let at = self.started.checked_add(*limit).ok_or_else(|| {
            ModelError::new(
                "config_invalid",
                "Response header deadline exceeds the monotonic clock range",
            )
        })?;
        let timeout = || {
            ModelError::new(
                "timeout",
                format!(
                    "HTTP response-header wait timed out after {} ms (limit {} ms; {}); request delivery is not inferred from this deadline",
                    self.started.elapsed().as_millis(),
                    limit.as_millis(),
                    setting
                ),
            )
        };
        tokio::pin!(future);
        // Do not start polling a new outgoing request after its wait expired.
        // Once admitted on time, later ready headers retain classification priority.
        let first = std::future::poll_fn(|cx| {
            std::task::Poll::Ready(if cancel.is_cancelled() {
                Err(cancelled())
            } else if Instant::now() >= at {
                Err(timeout())
            } else {
                Ok(future.as_mut().poll(cx))
            })
        })
        .await?;
        if let std::task::Poll::Ready(result) = first {
            return Ok(result);
        }
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(cancelled()),
            // Preserve ready status facts instead of inventing an unknown timeout.
            result = &mut future => Ok(result),
            _ = tokio::time::sleep_until(at) => Err(timeout()),
        }
    }

    pub(super) fn received(&mut self) {
        self.received = Instant::now();
    }
    pub(super) fn received_at(&self) -> std::time::Instant {
        self.received.into_std()
    }
    pub(super) async fn wait<F: Future>(
        &self,
        cancel: &CancellationToken,
        stage: &str,
        future: F,
    ) -> Result<F::Output, ModelError> {
        let deadline = match self.kind {
            DeadlineKind::Disabled => None,
            _ => Some(
                (if matches!(self.kind, DeadlineKind::Inactivity(_)) {
                    self.received
                } else {
                    self.started
                })
                .checked_add(self.kind.limit())
                .ok_or_else(|| {
                    ModelError::new(
                        "config_invalid",
                        format!("{} exceeds the monotonic clock range", self.setting),
                    )
                })?,
            ),
        };
        if matches!(self.kind, DeadlineKind::Total { .. }) {
            return tokio::select! {
                biased;
                _ = cancel.cancelled() => Err(cancelled()),
                _ = tokio::time::sleep_until(deadline.expect("total requests always have a deadline")) => Err(self.timeout(stage)),
                result = future => Ok(result),
            };
        }
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(cancelled()),
            // A ready byte wins after local consumer backpressure.
            result = future => Ok(result),
            _ = async { if let Some(at)=deadline {tokio::time::sleep_until(at).await} else {std::future::pending::<()>().await} } => Err(self.timeout(stage)),
        }
    }
    fn timeout(&self, stage: &str) -> ModelError {
        let inactivity = matches!(
            self.kind,
            DeadlineKind::Inactivity(_) | DeadlineKind::Disabled
        );
        let elapsed = if inactivity {
            self.received.elapsed()
        } else {
            self.started.elapsed()
        };
        let advice = if inactivity {
            "set 0 to disable inactivity timeout".to_owned()
        } else if matches!(
            self.kind,
            DeadlineKind::Total {
                endpoint: ApiEndpoint::Models,
                ..
            }
        ) {
            format!("model list also has a fixed {MODEL_CATALOG_TIMEOUT_MS} ms cap")
        } else {
            "adjust the connection request timeout if needed".to_owned()
        };
        ModelError::new(
            "timeout",
            format!(
                "HTTP {} timed out {stage} after {} ms (limit {} ms; {}; {})",
                if inactivity {
                    "receive inactivity"
                } else {
                    "request"
                },
                elapsed.as_millis(),
                self.kind.limit().as_millis(),
                self.setting,
                advice
            ),
        )
    }
}

pub(super) fn transport_error(error: reqwest::Error, stage: &str) -> ModelError {
    let cause = transport_cause(&error);
    let (kind, mut message) = if error.is_timeout() {
        (
            "timeout",
            format!("HTTP connection timed out {stage} (fixed connect_timeout 30000 ms)"),
        )
    } else {
        ("transport", format!("HTTP transport failed {stage}"))
    };
    use crate::model::TransportCause::*;
    let detail = match cause {
        Dns => "DNS lookup failed",
        ConnectionRefused => "connection refused",
        ConnectionReset => "connection reset",
        Unreachable => "network or host unreachable",
        TlsUnknownIssuer => "TLS certificate issuer is not trusted",
        TlsNameMismatch => "TLS certificate does not match the server name",
        TlsExpired => "TLS certificate has expired",
        Tls => "TLS negotiation or verification failed",
        ProxyConnect { status: Some(407) } => "proxy CONNECT requires authentication (407)",
        ProxyConnect { .. } => "proxy CONNECT failed (status unavailable)",
        Unknown => "underlying transport cause unavailable",
    };
    message.push_str(". ");
    message.push_str(TRANSPORT_HINT);
    message.push(' ');
    message.push_str(detail);
    let mut error = ModelError::new(kind, message);
    error.transport_cause = Some(cause);
    error
}

fn transport_cause(error: &(dyn std::error::Error + 'static)) -> crate::model::TransportCause {
    use crate::model::TransportCause::*;
    use rustls::{CertificateError, Error};
    use std::io::ErrorKind;
    let mut source = Some(error);
    let mut proxy = false;
    while let Some(error) = source {
        if let Some(tls) = error.downcast_ref::<Error>() {
            return match tls {
                Error::InvalidCertificate(CertificateError::UnknownIssuer) => TlsUnknownIssuer,
                Error::InvalidCertificate(
                    CertificateError::NotValidForName
                    | CertificateError::NotValidForNameContext { .. },
                ) => TlsNameMismatch,
                Error::InvalidCertificate(
                    CertificateError::Expired | CertificateError::ExpiredContext { .. },
                ) => TlsExpired,
                _ => Tls,
            };
        }
        // The pinned connector's DNS and tunnel errors are private. Match only
        // its fixed markers; never persist or expose any source Display text.
        let marker = error.to_string();
        match marker.as_str() {
            "dns error" => return Dns,
            "tunnel error: proxy authorization required" => {
                return ProxyConnect { status: Some(407) };
            }
            _ => proxy |= marker.starts_with("tunnel error: "),
        }
        if let Some(io) = error.downcast_ref::<std::io::Error>() {
            match io.kind() {
                ErrorKind::ConnectionRefused => return ConnectionRefused,
                ErrorKind::ConnectionReset => return ConnectionReset,
                ErrorKind::NetworkUnreachable | ErrorKind::HostUnreachable => return Unreachable,
                _ => {}
            }
            if let Some(inner) = io.get_ref() {
                source = Some(inner);
                continue;
            }
        }
        source = error.source();
    }
    if proxy {
        ProxyConnect { status: None }
    } else {
        Unknown
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn classified(client: reqwest::Client, url: &str) -> serde_json::Value {
        let error = crate::test_watchdog::watchdog("transport cause", client.get(url).send())
            .await
            .unwrap()
            .unwrap_err();
        serde_json::to_value(transport_error(error, "before response headers")).unwrap()
    }

    #[tokio::test]
    async fn transport_cause_identifies_dns_and_redacts_the_url() {
        struct FailingDns;
        impl reqwest::dns::Resolve for FailingDns {
            fn resolve(&self, _: reqwest::dns::Name) -> reqwest::dns::Resolving {
                Box::pin(async { Err(std::io::Error::other("private-resolver-info").into()) })
            }
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .dns_resolver(std::sync::Arc::new(FailingDns))
            .build()
            .unwrap();
        let error = classified(client, "http://private-host.invalid/a?key=private-query").await;
        assert_eq!(error["transport_cause"]["kind"], "dns");
        assert!(error["message"].as_str().unwrap().contains(
            "Check that the network or VPN connection to this endpoint is up, then inspect proxy connectivity and TLS settings."
        ));
        assert!(!error.to_string().contains("private-"));
    }

    #[tokio::test]
    async fn transport_cause_distinguishes_refusal_and_reset() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let client = reqwest::Client::builder()
            .no_proxy()
            .retry(reqwest::retry::never())
            .build()
            .unwrap();
        let reset = async {
            use tokio::io::AsyncReadExt;
            let (mut stream, _) = listener.accept().await.unwrap();
            assert_eq!(stream.read_u8().await.unwrap(), b'G');
            stream.set_zero_linger().unwrap();
        };
        let (error, ()) = tokio::join!(classified(client.clone(), &url), reset);
        assert_eq!(error["transport_cause"]["kind"], "connection_reset");
        drop(listener);
        let error = classified(client, &url).await;
        assert_eq!(error["transport_cause"]["kind"], "connection_refused");
    }

    #[tokio::test]
    async fn transport_cause_preserves_only_known_proxy_connect_status() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        for (returned, reported) in [
            (407, serde_json::json!(407)),
            (403, serde_json::Value::Null),
        ] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let proxy =
                reqwest::Proxy::all(format!("http://{}", listener.local_addr().unwrap())).unwrap();
            let client = reqwest::Client::builder().proxy(proxy).build().unwrap();
            let respond = async {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut line = [0; 8];
                stream.read_exact(&mut line).await.unwrap();
                assert_eq!(&line, b"CONNECT ");
                let mut tail = Vec::new();
                while !tail.ends_with(b"\r\n\r\n") {
                    tail.push(stream.read_u8().await.unwrap());
                    assert!(tail.len() < 4096);
                }
                stream
                    .write_all(
                        format!("HTTP/1.1 {returned} private-reason\r\nContent-Length: 0\r\n\r\n")
                            .as_bytes(),
                    )
                    .await
                    .unwrap();
                stream.shutdown().await.unwrap();
            };
            let (error, ()) = tokio::join!(
                classified(client, "https://private-target.invalid"),
                respond
            );
            assert_eq!(error["transport_cause"]["kind"], "proxy_connect");
            assert_eq!(error["transport_cause"]["status"], reported);
            assert!(!error.to_string().contains("private-"));
        }
    }

    #[test]
    fn transport_cause_reports_unreachable_for_network_and_host_errors() {
        use std::io::{Error, ErrorKind};
        for kind in [ErrorKind::NetworkUnreachable, ErrorKind::HostUnreachable] {
            let io = Error::other(Error::from(kind));
            let cause = serde_json::to_value(transport_cause(&io)).unwrap();
            assert_eq!(cause["kind"], "unreachable");
        }
    }

    #[test]
    fn transport_cause_keeps_typed_tls_certificate_reasons() {
        use rustls::CertificateError;
        for (source, expected) in [
            (CertificateError::NotValidForName, "tls_name_mismatch"),
            (CertificateError::Expired, "tls_expired"),
            (CertificateError::UnknownIssuer, "tls_unknown_issuer"),
        ] {
            let io = std::io::Error::other(rustls::Error::InvalidCertificate(source));
            let cause = serde_json::to_value(transport_cause(&io)).unwrap();
            assert_eq!(cause["kind"], expected);
        }
    }

    #[tokio::test(start_paused = true)]
    async fn ready_permanent_headers_are_not_erased_by_a_simultaneously_expired_wait() {
        for status in [400_u16, 401, 403, 422] {
            let connection = Connection {
                response_header_timeout_ms: 500,
                stream_idle_timeout_ms: 500,
                ..Default::default()
            };
            let deadline = Deadline::new(ApiEndpoint::Responses, &connection, "fixture").unwrap();
            let cancel = CancellationToken::new();
            let (send, receive) = tokio::sync::oneshot::channel();
            let waiting = deadline.wait_headers(&cancel, receive);
            tokio::pin!(waiting);
            assert!(futures_util::poll!(&mut waiting).is_pending());
            tokio::time::advance(Duration::from_millis(500)).await;
            send.send(status).unwrap();
            assert_eq!(
                waiting.await.unwrap().unwrap(),
                status,
                "a scheduler-late poll must classify available permanent headers"
            );
            cancel.cancel();
            assert_eq!(
                deadline
                    .wait_headers(&cancel, std::future::ready(status))
                    .await
                    .unwrap_err()
                    .kind,
                "cancelled"
            );
            assert_eq!(
                deadline
                    .wait_headers(&CancellationToken::new(), std::future::pending::<u16>())
                    .await
                    .unwrap_err()
                    .kind,
                "timeout",
                "pending headers still expire"
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn expired_initial_header_wait_does_not_poll_a_new_request() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let config = Connection {
            response_header_timeout_ms: 500,
            ..Default::default()
        };
        let deadline = Deadline::new(ApiEndpoint::Responses, &config, "fixture").unwrap();
        tokio::time::advance(Duration::from_millis(500)).await;
        let polled = Arc::new(AtomicBool::new(false));
        let observed = polled.clone();
        let outgoing = async move {
            observed.store(true, Ordering::SeqCst);
            400_u16
        };
        assert_eq!(
            deadline
                .wait_headers(&CancellationToken::new(), outgoing)
                .await
                .unwrap_err()
                .kind,
            "timeout"
        );
        assert!(
            !polled.load(Ordering::SeqCst),
            "an expired entry must not initiate another outgoing request"
        );
    }

    #[tokio::test]
    async fn expired_total_budget_rejects_ready_data_and_cancel_keeps_priority() {
        let config = Connection {
            request_timeout_ms: 1000,
            ..Default::default()
        };
        let mut deadline = Deadline::new(ApiEndpoint::Embeddings, &config, "embed").unwrap();
        deadline.started -= Duration::from_secs(2);
        let cancel = CancellationToken::new();
        let error = deadline
            .wait(&cancel, "receiving response body", std::future::ready(()))
            .await
            .unwrap_err();
        assert_eq!(error.kind, "timeout");
        assert!(
            error
                .message
                .contains("connections.embed.request_timeout_ms")
        );
        cancel.cancel();
        assert_eq!(
            deadline
                .wait(&cancel, "headers", std::future::ready(()))
                .await
                .unwrap_err()
                .kind,
            "cancelled"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn heartbeat_resets_idle_deadline_on_a_controlled_clock() {
        let config = Connection {
            stream_idle_timeout_ms: 1000,
            request_timeout_ms: 1000,
            ..Connection::default()
        };
        let mut deadline = Deadline::new(ApiEndpoint::Responses, &config, "heartbeat").unwrap();
        let cancel = CancellationToken::new();
        for _ in 0..6 {
            {
                let heartbeat = deadline.wait(
                    &cancel,
                    "heartbeat",
                    tokio::time::sleep(Duration::from_millis(250)),
                );
                tokio::pin!(heartbeat);
                assert!(futures_util::poll!(&mut heartbeat).is_pending());
                tokio::time::advance(Duration::from_millis(250)).await;
                heartbeat.await.unwrap();
            }
            deadline.received();
        }
        let idle = deadline.wait(&cancel, "no more heartbeat", std::future::pending::<()>());
        tokio::pin!(idle);
        assert!(futures_util::poll!(&mut idle).is_pending());
        tokio::time::advance(Duration::from_millis(1000)).await;
        assert_eq!(idle.await.unwrap_err().kind, "timeout");
    }

    #[test]
    fn catalog_cap_and_disabled_stream_do_not_change_other_endpoint_budgets() {
        let config = Connection {
            stream_idle_timeout_ms: 0,
            request_timeout_ms: 90_000,
            ..Default::default()
        };
        let models = Deadline::new(ApiEndpoint::Models, &config, "one").unwrap();
        assert_eq!(models.kind.limit(), Duration::from_secs(20));
        let embeddings = Deadline::new(ApiEndpoint::Embeddings, &config, "one").unwrap();
        assert_eq!(embeddings.kind.limit(), Duration::from_secs(90));
        assert!(!matches!(embeddings.kind, DeadlineKind::Disabled));
        let responses = Deadline::new(ApiEndpoint::Responses, &config, "one").unwrap();
        assert!(matches!(responses.kind, DeadlineKind::Disabled));
        let short = Connection {
            request_timeout_ms: 1000,
            ..config
        };
        assert_eq!(
            Deadline::new(ApiEndpoint::Models, &short, "two")
                .unwrap()
                .kind
                .limit(),
            Duration::from_secs(1)
        );
    }
}
