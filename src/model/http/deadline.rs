//! Per-request transport deadlines, shared by headers and every body reader.
use super::*;
use std::{future::Future, time::Duration};
use tokio::time::Instant;

pub(super) struct Deadline {
    started: Instant,
    received: Instant,
    body_received: bool,
    limit: Duration,
    idle: bool,
    disabled: bool,
    catalog: bool,
    setting: String,
}
impl Deadline {
    pub(super) fn new(
        endpoint: ApiEndpoint,
        connection: &Connection,
        name: &str,
    ) -> Result<Self, ModelError> {
        let idle = matches!(endpoint, ApiEndpoint::Responses);
        let (milliseconds, key) = if idle {
            (connection.stream_idle_timeout_ms, "stream_idle_timeout_ms")
        } else {
            (
                if matches!(endpoint, ApiEndpoint::Models) {
                    connection.request_timeout_ms.min(20_000)
                } else {
                    connection.request_timeout_ms
                },
                "request_timeout_ms",
            )
        };
        let now = Instant::now();
        let limit = Duration::from_millis(milliseconds);
        let setting = format!("connections.{name}.{key}");
        if now.checked_add(limit).is_none() {
            return Err(ModelError::new(
                "config_invalid",
                format!("{setting} exceeds the monotonic clock range"),
            ));
        }
        Ok(Self {
            started: now,
            received: now,
            body_received: false,
            limit,
            idle,
            disabled: idle && milliseconds == 0,
            catalog: matches!(endpoint, ApiEndpoint::Models),
            setting,
        })
    }
    pub(super) fn received(&mut self) {
        self.received = Instant::now();
    }
    pub(super) fn received_body(&mut self) {
        self.body_received = true;
        self.received();
    }
    pub(super) fn has_body(&self) -> bool {
        self.body_received
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
        let deadline = if self.disabled {
            None
        } else {
            Some(
                if self.idle {
                    self.received
                } else {
                    self.started
                }
                .checked_add(self.limit)
                .ok_or_else(|| {
                    ModelError::new(
                        "config_invalid",
                        format!("{} exceeds the monotonic clock range", self.setting),
                    )
                })?,
            )
        };
        if !self.idle {
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
            // A ready byte wins over an idle timer after local consumer backpressure.
            result = future => Ok(result),
            _ = async { if let Some(at) = deadline { tokio::time::sleep_until(at).await } else { std::future::pending::<()>().await } } => {
                Err(self.timeout(stage))
            }
        }
    }
    fn timeout(&self, stage: &str) -> ModelError {
        let elapsed = if self.idle {
            self.received.elapsed()
        } else {
            self.started.elapsed()
        };
        ModelError::new(
            "timeout",
            format!(
                "HTTP {} timed out {stage} after {} ms (limit {} ms; {}; {})",
                if self.idle {
                    "receive inactivity"
                } else {
                    "request"
                },
                elapsed.as_millis(),
                self.limit.as_millis(),
                self.setting,
                if self.idle {
                    "set 0 to disable inactivity timeout"
                } else if self.catalog {
                    "model list also has a fixed 20000 ms cap"
                } else {
                    "adjust the connection request timeout if needed"
                }
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
        assert_eq!(models.limit, Duration::from_secs(20));
        let embeddings = Deadline::new(ApiEndpoint::Embeddings, &config, "one").unwrap();
        assert_eq!(embeddings.limit, Duration::from_secs(90));
        assert!(!embeddings.disabled);
        let responses = Deadline::new(ApiEndpoint::Responses, &config, "one").unwrap();
        assert!(responses.disabled);
        let short = Connection {
            request_timeout_ms: 1000,
            ..config
        };
        assert_eq!(
            Deadline::new(ApiEndpoint::Models, &short, "two")
                .unwrap()
                .limit,
            Duration::from_secs(1)
        );
    }
}
