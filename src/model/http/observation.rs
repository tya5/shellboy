//! Bounded transport observations. Consuming a source is not proof of delivery.
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HttpPhase {
    /// Connection setup failed or the body source was never consumed.
    BeforeBodySource,
    UploadSourceIncomplete,
    AwaitingHeadersDeliveryUnknown,
    BodyReceive,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnectionReuse {
    /// First request issued from a newly constructed client pool.
    FreshPool,
    /// The public transport API does not expose the actual socket's reuse.
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpObservation {
    pub phase: HttpPhase,
    pub request_body_bytes: usize,
    pub body_source_bytes_consumed: usize,
    pub header_elapsed_ms: Option<u64>,
    pub pool_generation: u64,
    pub connection_reuse: ConnectionReuse,
}
impl HttpObservation {
    pub fn is_known_upload_incomplete(&self) -> bool {
        self.phase == HttpPhase::UploadSourceIncomplete
            && self.body_source_bytes_consumed > 0
            && self.body_source_bytes_consumed < self.request_body_bytes
    }
    pub(super) fn hint(&self) -> &'static str {
        match self.phase {
            HttpPhase::BeforeBodySource => {
                "Connection setup failed or no request-body source bytes were consumed; no partial upload was observed. Check endpoint connectivity, DNS and TLS; peer delivery and provider processing are unknown"
            }
            HttpPhase::UploadSourceIncomplete => {
                "The HTTP client has not consumed the complete request body; complete delivery cannot be established. Check the connection, forwarding proxy and local network; provider processing is unknown"
            }
            HttpPhase::AwaitingHeadersDeliveryUnknown => {
                "Awaiting response headers; the HTTP client consumed the body source but peer delivery and processing are unknown"
            }
            HttpPhase::BodyReceive => "Response headers arrived; response-body reception stopped",
        }
    }
}
