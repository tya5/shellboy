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

## Source Child: Bounded Unknown Emit, Not Adoption Proof

Guard admission now allocates its existing incarnation before ordinary fragments,
which pass through Producer::begin/admit_ordinary/freeze. V1 fragments preserve
original boundaries and projection offsets; legacy offsets remain mixed scalars.
Metadata, hash state and one seal reserve are charged within the existing64KiB;
fragment heap counters are included and eviction precedes retained body allocation.
Ordinary-only SHA is not a producer part/adoption proof: ALL seals in this child
are Unknown, never Complete or committed Positive. No public serde shape changed.

Freeze occurs at response future termination. Observed-channel drain/source-part
provenance is not yet qualified. Seal exists only while the existing live guard
owns that attempt and expires on its existing drop/terminal clear; a waiter may
miss it, yielding Unknown/unavailable, never a fabricated retained terminal proof.
This intentionally does NOT introduce a terminal history map or promise reliable
post-guard delivery. Successful delivery lifetime needs a separate qualified
adoption/owner boundary before positive publication. No compaction/adoption SQL,
policy, migration or saved witness is modified by this child.

The added lifecycle/overflow fixtures are source-only, NOT compiled or executed
under memory hold; format/toolchain checks are also unexecuted.
