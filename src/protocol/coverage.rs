//! Ordinary-text V1 coverage wire types. Producer and consumer validation remains mandatory.
use super::*;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CoverageIdentityV1 {
    pub version: u8,
    pub projection: CoverageProjection,
    pub thread_id: String,
    pub attempt_id: String,
    pub incarnation: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CoverageProjection {
    OrdinaryTextV1,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CoverageFragmentV1 {
    pub sequence: String,
    pub scalar_offset: String,
    pub stream_kind: CoverageStreamKind,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CoverageStreamKind {
    Text,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CoverageSealV1 {
    pub identity: CoverageIdentityV1,
    pub next_sequence: String,
    pub scalar_end: String,
    pub utf8_bytes: String,
    pub transcript_sha256: String,
    pub disposition: CoverageSealDisposition,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CoverageSealDisposition {
    Complete,
    Unknown,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AttemptCoverageV1 {
    pub seal: CoverageSealV1,
    pub adoption: CoverageAdoption,
    pub answer_member_count: String,
    pub presentation: CoveragePresentation,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CoverageAdoption {
    CoveredNonempty,
    CoveredEmpty,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CoveragePresentation {
    CanonicalAnswer,
    ExplicitNoAnswer,
}
