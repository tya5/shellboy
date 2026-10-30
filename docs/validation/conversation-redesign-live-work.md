# Live work qualification (#41 D2)

This increment adds the selected Run's bounded shell/child/wait display and
reactive output observer. It does not qualify E list patches, F Watch/descendants,
G forms/popups, combined multi-consumer P3 or the whole #41 issue.

## Implementation boundaries

- The existing current-view DB transaction captures counts and the oldest three
  saved owned work rows. Missing timing stays unknown. Wait observations use at
  most128 exact registry operation/source pairs and saved ownership; absent
  metadata differs from malformed proof or an unavailable registry snapshot.
- Child classification reuses the existing classifier. Only the final three
  display candidates may use bounded title witnesses, at most two additional
  statements per untitled child inside that same transaction. This is not a
  set-oriented128-Thread browser title implementation.
- Existing postcommit Changed notifications cover pending wait insertion/removal,
  new owned Run receipt effects and child recovery/completion. Only the accepted
  selected projection's maximum three child Thread IDs become display dependencies;
  unrelated hidden changes do not start hidden history reads. Existing500ms
  current-view admission coalesces parent changes.
- The live shell registry holds one optional monotonic committed-output marker per
  already-live handle. Failed output writes do not update it or notify output.
  No new table, queue, worker, business event, timer-driven DB poller or migration
  is added.
- Only one selected/open shell reads raw output, through the existing finite
  read/presentation admission. One500ms deadline survives close/open and selection
  changes. Each read copies at most4KiB total raw data and inspects128 chunk
  metadata rows. Separate stream decoders preserve contiguous partial code units;
  backlog/retention/encoding changes reset to an explicitly abbreviated tail.
- The frontend cursor retains at most4KiB decoded text and the active UI preview
  at most4KiB. This explicit8KiB retained consumer allowance excludes bounded
  temporary delivery/preparation copies and decoder/identity metadata. It is
  separate from history cache.body_bytes and is not an RSS cap. Closing/hiding
  or switching discards active output, and stale results cannot rehydrate it.
- Saved last-output age and postcommit local observation age have distinct labels.
  Display clocks advance locally without draft autosave, source reparsing or DB
  actions; this structural assertion is not a native CPU measurement.

## Validation state

The initial strict all-target Clippy passed after compile corrections. The first
full standard run failed three assertions: two product regressions (initial
output admission and idle-clock save responsibility) and a wide-cell screen
reconstruction defect in the overflow test. The second run isolated that harness
failure at886/1/13. Product fixes and accurate Unicode reconstruction preserve
the substantive assertions. Another full run passed888 library tests and found
one intentional background-cadence integration expectation; preserve its state
label/spawn-count assertions while making the return flag's poll meaning explicit.

Source45b8b15 passes native macOS arm64/Rust1.99.1 `sh scripts/check.sh`:
**1,775 passed/0 failed/32 ignored** (library889, integration876, termination7,
crash3), layout90, Python6, formatting and strict all-target Clippy. Before that
standard run,81 focused tests passed/0 failed/1 ignored. This source also fixes
the delayed-presentation snapshot clock and retains bounded cursor allocations.
The separate latest-value delivery follow-up78511d4 and bundled help/guides
807ac97 then pass the complete native standard check again: **1,777/0/32**
(library891, integration876, termination7, crash3), layout90/Python6/fmt/strict
all-target Clippy. The final source is807ac97. A full raw UI channel plus1,000
updates of each live kind retains two latest entries and zero critical entries;
complete marker snapshots preserve earlier observations for another execution.
No Windows/Linux native result or P2/P5 acceptance is claimed by these checks.

Native synthetic PTY scenarios, final follow-up results, source identity and
remaining gates are recorded here after execution. The installed executable and
user's default data are outside these tests.

## Native selected-shell PTY observation

On native macOS arm64, run the compiled development executable in six independent
40-row PTYs:80/140 columns times dark/light/NO_COLOR. Every fixture owns a fresh
synthetic data directory and loopback-only Responses server. A real stdin-gated
background shell emits its initial bytes, waits for a separate CLI write, emits
new output and is stopped through the CLI. The model's answer finishes before
the background observation. All six exit0 with no mock-server error:

| Observation | Result |
| --- | --- |
| Completed Turn retains its live owned shell above the composer |6/6 |
| Shift+Tab/Space opens a real retained output tail |6/6 |
| CLI stdin write updates the open preview without a new model answer/input |6/6 |
| Stop removes the work row after committed terminal change |6/6 |
| F1 coverage and resume captures |6/6 captured; late-reply rejection separately tested |
| NO_COLOR emits no chromatic SGR |2/2 |

All scenarios use frozen executable SHA256
`8156737dc8a810d8daae99ff45a6376ca2b3579c8aa66c6d9cff573c4f157fc2`.
Cells are reconstructed from the actual PTY bytes, not a replacement renderer.
Native GUI/font screenshots and Windows/Linux terminal observations were not
performed. Native child/wait interactions are not established by this shell-only
fixture; their ownership/phase/timing and typed targets have saved-data/UI tests.

Each scenario observes only three silent seconds. Corresponding PTY bytes per
second are188(dark/light80),136(plain80),190(dark/light140),138(plain140). These are
transport sizes, not draw-call or DB-job counts and not a ten-minute CPU result.
They do not qualify P2 or hidden-output40batch/sec P5. Retain the actual synthetic
captures privately; generated files and personal paths are excluded from Git.
