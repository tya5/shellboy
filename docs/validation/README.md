# Validation entry point

Record environments and observed results in [acceptance](acceptance.md).
Use the [manual terminal procedures](manual-terminal.md) for interactive checks.

Run standard checks from the repository root with `sh scripts/check.sh`
(PowerShell: `./scripts/check.ps1`). No model API or private data is required.

Additional synthetic fixtures and verification tools live in `tests/`, `examples/`
and `scripts/`. Keep generated artifacts in ignored locations such as `target/`.
Do not include real conversations, personal paths or credentials in public records.

Conversation redesign evidence is split into [B+C](conversation-redesign-bc.md),
[shared reads](conversation-redesign-shared-read.md), and
[live work](conversation-redesign-live-work.md) and
[E list qualification](conversation-redesign-e.md) and
[F Watch qualification](conversation-redesign-f.md). Each records its own source,
observations and remaining gates; evidence is not transferable between increments.
