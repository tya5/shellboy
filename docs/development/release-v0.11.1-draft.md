# Shellboy v0.11.1

Preparation draft after [v0.11.0](https://github.com/tya5/shellboy/releases/tag/v0.11.0),
not a release announcement. Source scope is only
`ff18f32640f0b41f042f034f4b1a3936a9abe92d` ([#286](https://github.com/tya5/shellboy/issues/286))
plus package version/lock metadata and these preparation records. No other feature
is included. Tag/publication and installed-binary replacement are separate actions.

## Breaking changes

None. Protocol remains 5; conversation schema remains 39. No migration, new RPC,
configuration or permission change is introduced by this patch.

## Fix

Saved assistant history may contain text tagged `input_text`, for example a
persisted human-approval question. Replaying it to a strict Responses provider
could be rejected. Outbound replay now converts assistant `input_text` parts to
`output_text`. Stored history is not rewritten; ordinary user `input_text`, existing
assistant `output_text` and `refusal` stay unchanged. This does not claim recovery
of any existing affected Thread or validation against a paid/real provider.

## Feature evidence (four columns)

The validation column distinguishes implementation evidence from candidate release
qualification. Pending means unverified, not failed. Earlier-SHA CI is not this
version commit's ordinary three-OS CI.

| Feature / evidence | Designed | Implemented | Validation / limitations |
| --- | --- | --- | --- |
| #286 assistant replay normalization; `ff18f32640`; `control::replay::tests::replayed_assistant_input_text_is_normalized_for_responses_without_touching_user_text` | Yes: normalize outbound copy only, preserve saved/user content | Yes: replay normalization and synthetic Responses encoder regression | Source regression inspected; execution is not newly claimed by these notes. Final version-commit three-OS CI, exact-main candidate build and public-v0.11.0-to-candidate upgrade are pending. Real provider and affected real-Thread recovery are unverified. |

## Before upgrading

- Stop your owner in a separately coordinated maintenance action, back up the
  complete data directory, update owner and CLI together, then restart. This
  preparation does not stop, replace or inspect any installed owner or user data.
- Public v0.11.0 and this candidate both use schema 39. Same-schema startup and
  preserved synthetic configuration/history still need the public-binary check;
  absence of a migration is not proof of successful upgrade or downgrade.
- Candidate qualification must use exact main after its ordinary three-OS checks
  succeed. Candidate artifacts/checksums and execution results are separate facts.
  The leader performs the final tag/publication; no tag is created by this draft.

## Upgrade qualification plan (not executed)

Use only the public v0.11.0 archive for each target OS plus its published
`SHA256SUMS`, and exact-main v0.11.1 candidate assets plus their checksum manifest.
Verify each archive, extract to an isolated directory, and check both binary
versions. Feed the old and new binaries to the existing
`scripts/check-release-upgrade.py` with a fresh non-existent temporary fixture
root, old version 0.11.0, start/new schema 39 and expected version 0.11.1. Use only
that script's loopback mock and synthetic data; never an installed data directory.

The helper now accepts public 0.11.0 with schema 39, verified against the published
v0.11.0 tag `4dc8f66a6c586db57a7a63a3a8be84e25999c95f` and current migration
list (`0039_independent_runs`). Its historical default old version stays 0.10.0;
pass 0.11.0 explicitly. Parser/source tests cover that entry and preserve rejection
of an invalid schema rollback; they are not a public-binary upgrade execution.
Record actual
source SHA, native OS, asset checksums, command and observed result outside Git
rather than making recursive notes-only version commits. Failure stops the train;
no independent rerun or publication is implied by this plan.
