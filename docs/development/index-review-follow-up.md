# Index review integration (#42/#43/#55)

The reviews at [#42](https://github.com/tya5/shellboy/issues/42#issuecomment-6514575509),
[#43](https://github.com/tya5/shellboy/issues/43#issuecomment-6514575837), and
[#55](https://github.com/tya5/shellboy/issues/55#issuecomment-6514576234) report no
Medium findings. The user authorizes main publication after current-main
integration and standard checks. Keep their suggestions as assessed evidence,
not an automatic replacement of the adopted design.

| Finding | Disposition | Verification / revisit condition |
| --- | --- | --- |
| #42 PTY display guards are 20 seconds | Record only; no speculative timer change. This test is Unix-only, so it does not establish or currently fail a Windows physical-terminal case. | Native macOS/Linux have passed; if a readiness guard flakes, use the established 60-second deadlock bound and state diagnostics. |
| #43 a transient aggregate failure permanently marks its current inputs singleton | Retain the safe deterministic fallback; no heuristic rebatching or reset after a guessed recovery. | Existing fallback/split/retry regressions remain. Revisit only with a separately adopted retry policy and evidence. |
| #43 larger batches may need a larger whole-request deadline | Adopt guide clarification. Raising batch count requires reviewing request_timeout_ms and proxy caps; one deadline covers the entire batch. | Configuration guide is included in the checked combined tree. No default or request algorithm changes. |
| #55 smaller payload does not immediately shrink SQLite/WAL | Retain existing explicit Agent Guide/design caveat. Do not describe vector arithmetic as physical disk reclamation. | Explicit GC and protected-space/rollback regressions remain. Real provider quality/support remains untested. |

Merge51a819f incorporates main5350fc8 usage work while preserving both model HTTP
test sets and documentation. Native macOS standard checks succeed: 1,504 passed,
zero failed, 28 ignored; six Python gates, 85 suites, formatting and all-target
strict Clippy. Publish to main and verify its exact-source native CI before
closing the issues. Earlier ae9bc19 branch CI is separate evidence, not proof of
this newly combined tree. No release, installed-binary update or user-data
reindex/GC is included.
