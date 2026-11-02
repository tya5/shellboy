# Release Candidate Reuse

Status: **expiry policy A approved**; implementation and non-publishing tests
are authorized. Issue #161 applies after the 0.6.1 publication. Actual release
publication is not authorized by this decision.

## Existing Behavior And Constraints

`release.yml` builds three native archives, assembles version-stamped installers
and `SHA256SUMS` in `candidate-assets` (30-day retention), and runs candidate
installer checks when `candidate_ref` is set. An ordinary tag publication builds
and assembles again. A rebuild is not necessarily byte-identical, notably on
Windows. The release workflow's run `head_sha` identifies the workflow dispatch
ref; it does not by itself prove which `candidate_ref` was checked out. Historical
candidate artifacts do not contain a machine-verifiable binding of candidate SHA,
version label and run ID. A successful release build also does not prove the
separate three-OS CI or the human's upgrade/acceptance checks passed.

The reviewer continues to run three-OS CI and candidate builds in parallel on
the same exact SHA. A failed CI disqualifies the candidate. A successful candidate
workflow requires all three native build/smoke jobs, assembly and all installer
checks; separate CI, upgrade qualification and publication approval remain required.
Nothing in this proposal authorizes tagging, release creation or actual publication.

## Candidate Identity And Selection

- New candidate runs produce a separate small provenance artifact identifying
  repository, release workflow, candidate run ID and attempt, exact checked-out
  40-digit SHA, version label, the `candidate-assets` artifact ID and the digest
  of `SHA256SUMS`. Generate it from the checked-out tree and GitHub runtime
  context, not a user-provided assertion alone. Keep its retention longer than
  `candidate-assets` (proposed: 90 versus 30 days).
- Publication resolves the tag to its commit, validates the stable version and
  looks only at completed successful candidate workflow runs of this repository.
  Compare the provenance checkout SHA and version label exactly with that commit
  and tag, and confirm the expected candidate jobs and attempts succeeded. Never
  use a run's `head_sha` as a substitute for the candidate checkout SHA. Old runs
  without this provenance cannot be proven eligible and are treated as no match.
- Select the newest matching successful candidate by run creation time and run ID;
  this permits a replacement candidate after expiry without retagging. Download
  that run's exact `candidate-assets` by run and artifact ID. Multiple available
  matching candidates fail as ambiguous; an older expired candidate does not
  block its replacement. Verify the complete expected asset set, every SHA-256
  against `SHA256SUMS`, the recorded checksum-file digest, and tag-bound names
  before any write. Publish those very bytes, including the two installer scripts;
  do not rebuild, repackage, or regenerate checksums. Any mismatch, API failure,
  incomplete search, inconsistent eligible candidates or invalid metadata
  fails closed rather than silently publishing different bytes. Explicitly define
  pagination/bounds so a truncated search is never treated as no candidate.
- With no provably matching candidate, or candidates only for another SHA/version,
  use the existing build-and-publish path. A candidate selected for a different
  SHA is never repurposed. Version validation and checkout must also hold on the
  fallback path. Concurrent or rerun candidate attempts must not mix artifacts;
  if exact attempt provenance cannot be established, fail closed.
- Record the candidate run URL/ID, exact commit and full `SHA256SUMS` in the
  workflow's durable publication record and release body. For fallback, record
  that no candidate was reused and the publication-build run, commit and sums;
  never imply those bytes are the earlier candidate. Keep notes and release assets
  immutable once public, and preserve the existing draft-on-upload-failure safety.

## Expired Artifact: Approved Policy

**Approved A: fail explicitly** when matching candidate provenance exists
but its `candidate-assets` is expired, missing or cannot be downloaded. No release
is created or changed. Tell the operator the candidate run and reason; rerun `candidate.sh` with the
same SHA, validate the new candidate and rerun the release workflow. Do not
retag. This avoids
quietly replacing the qualified Windows executable with different bytes.

**Alternative B: rebuild** on expiry, marking the result and checksums as a new
build, not the validated candidate. This preserves availability but requires a
separate decision about whether and how the newly built artifacts must be validated
before publication. It does not satisfy byte identity with the earlier candidate.

The 90-day provenance / 30-day assets policy makes known expiry distinguishable
while provenance exists. After provenance itself expires or a legacy candidate has
none, the workflow cannot distinguish an old expired candidate from no candidate;
under the no-match rule it falls back to a new build. API errors or an incomplete
search are not evidence of absence. A longer-lived authoritative record would be
needed for an indefinite fail-on-expiry guarantee; no such record is assumed.

## Dry-Run And Tests

Add an explicit dry-run mode in which all routes have read-only permissions and
no operation creates/updates a tag, draft, Release or public asset. Dry-run must
exercise the same selection and verification logic, not a separate optimistic
simulation. Use synthetic local artifacts and a fake GitHub API or a non-publishing
workflow dispatch to cover: matching candidate and byte-for-byte asset identity;
no candidate and fallback build decision; different SHA and fallback; expired or
missing assets according to the approved policy; replacement after expiry; API pagination,
errors, truncated searches and corrupt/incomplete checksums; mismatched version,
run/attempt/provenance and rerun race; and release-body evidence generation.
Assert publish/write calls are impossible in all dry-run cases. These are mock or
non-publishing checks, not proof of a real public Release or provider availability.

Run the normal branch CI and non-publishing dry-run after implementation; preserve
exact command, SHA and result. Integrate only after 0.6.1 publication and review.
