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
        tokio::select! {
            biased;
            _ = cancel.cancelled() => Err(cancelled()),
            _ = tokio::time::sleep_until(at) => Err(ModelError::new("timeout", format!("HTTP response-header wait timed out after {} ms (limit {} ms; {}); request delivery is not inferred from this deadline", self.started.elapsed().as_millis(), limit.as_millis(), setting))),
            result = future => Ok(result),
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
    if error.is_timeout() {
        ModelError::new(
            "timeout",
            format!(
                "HTTP connection timed out {stage} (fixed connect_timeout 30000 ms); inspect proxy connectivity and TLS settings"
            ),
        )
    } else {
        ModelError::new(
            "transport",
            format!("HTTP transport failed {stage}; inspect proxy connectivity and TLS settings"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
