# Exact Core V1 Types Checkpoint

src/protocol/coverage.rs defines authoritative serde field spelling and closed enums.
Not yet attached to frames. Proposed adopted outer optional fields to connect next:
EventLiveText.coverage_v1: CoverageIdentityV1;
EventTextDelta.coverage_v1: CoverageFragmentV1 (text remains existing delta.text);
EventsWaitResult.coverage_seals_v1: Vec<CoverageSealV1> (absent when empty);
WorkItem.coverage_v1: AttemptCoverageV1 (exact model_attempt lookup only).
All optional fields omit None; terminal lane omits empty. Thread identity is inside
seal; no fabricated legacy live_end seal. Counters are canonical decimal strings,
version must equal1, incarnation canonical UUID. Validation is NOT serde alone.
No snapshot/coverage producer emits any of these yet. Storage number unassigned.
