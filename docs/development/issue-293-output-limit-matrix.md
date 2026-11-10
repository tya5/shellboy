# Issue 293: Output-Limit Reuse And Acceptance Matrix

## Provenance And Scope

Fresh task base: `45e91c6c5b` (origin/main at task preparation).
Reused completed implementation, without editing foreign worktrees:
`7337075d10`, `545d805c7c`, `385b9faf45`, `e0a238c370`, and documentation
`f264b29e33`. The older checkpoint-status commit `a5a4f8bfc7` is historical,
not a current acceptance report and is not copied into the new status page.

Remaining source-audit gaps were a stale Messages-only design paragraph,
README's API list, bilingual User Guide guidance, and explicit JSON negative /
serialized error-kind coverage. These are repaired here. The independent
Shellboy Studio README link is preserved. No provider query or credentials
were used. No protocol shape, schema version, DB, installed binary, or foreign
worktree changes are needed: `ModelError.kind` is already an open string.

## Matrix

| Case / type | Actual source path | Focused controls |
| --- | --- | --- |
| Configured limit, three APIs | `HttpModelClient::wire_request` -> `dialect::encode_tagged` | `actual_request_producer_maps_configured_limits_without_changing_other_fields` |
| Unset Responses / Chat omitted; Messages 32768 | Same request producer | Same three-API configured/unset control; compare all other fields unchanged |
| Config positive / unset saved readback | Config validation and real config edit | `config_parser_accepts_positive_limits_for_each_api_and_keeps_unset_optional`, `three_api_output_limit_is_saved_and_read_back_without_other_setting_changes` |
| Native duplicate equal / different / null | Config validator and send guard | `mapped_output_limit_collision_is_rejected_without_reserving_other_keys`; unset extra-body passthrough retained |
| Responses SSE and JSON exact stop | `ResponseStream`, `ModelResponse::completed` | `exact_output_limit_reasons_are_typed_and_keep_partial_text_for_each_dialect` |
| Chat SSE and JSON `length` | Dialect normalization -> shared completion | Same exact-stop control; `json_output_limit_preserves_exact_reason_and_does_not_enable_messages_json` |
| Messages SSE `max_tokens` | Messages normalization -> shared completion | Exact-stop control and integration `stop_reasons_complete_stay_incomplete_or_fail_without_guessing` |
| Messages JSON | Existing stream-only adapter rejects it | JSON negative control: protocol error, no new Messages nonstream mode |
| Content filter / context / unknown / EOF | Exact-match classification only | `unrelated_incomplete_reasons_and_transport_eof_are_not_output_limit`, JSON negative control |
| Partial text / raw audit, no completion | Existing response recovery paths | Exact-stop control preserves `partial_text` / failed raw; HTTP `raw_failure` retains JSON output on failure |
| No automatic retry of provider output stop | Existing open error-kind retry classifier | Exact-stop control asserts `RetryPolicy::Ineligible` |
| Structured kind / human hint | Existing serde `ModelError` and Display | JSON control checks `kind=output_limit` and `max_tokens` hint; no new schema enum |

The public legacy `encode_hinted` numeric parameter remains Messages-only for
compatibility. The actual connection-aware request producer applies the optional
setting to all APIs. That distinction is deliberate, not a second user setting.

## Validation Boundaries

Local validation uses one rust-slot, `cargo check --tests`, and focused controls
only. Exact commands, exit state, and CI URLs are recorded in the handoff, not
assumed from this source document. Full standard check and all three OSes run
in GitHub CI on the final reviewed SHA. No paid-provider, LiteLLM, model-info,
private proxy, or live model acceptance is claimed. No new output-limit default
is introduced for Responses or Chat Completions.
