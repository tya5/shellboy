# Native TUI qualification tooling

This is CI-only instrumentation, not a product feature. `tui-measure.py prepare`
validates a clean subject commit and every hook before editing a disposable
checkout. The manifest records original and modified source hashes, hook sites,
and the harness identity. Builds retain binary witnesses. Normal product source,
configuration, dependencies and installed binaries do not gain measurement hooks.

Use the manual `native_tui_performance` workflow inputs with full baseline and
candidate SHA values. Native Unix jobs run both subjects sequentially on the same
runner at80/140 columns. P2 uses one silent stdin-gated background shell for600s;
P5 and P5-list use three independent50-line/s producers for60s. Actual committed
output rate is recorded; a target rate alone does not qualify the40-batch/s load.
The measurement window has no CLI probes or model requests, and an ACK pipe
identifies its actual start/end. CPU/draw/read values are observations, not a
runner-speed pass/fail assertion; fixture/source/build failures fail the job.

Subjects that actually contain Watch also get two independent scenarios:

- P2-watch: selected parent, actual live child/background shell visible in scoped
  Watch, one silent producer,600s. Visible clock redraws are expected and measured.
- P5-watch: selected quiet root with no descendants, scoped empty Watch, producer
  Threads outside that root,60s. The saved parent membership is checked before
  starting the window. The output load must remain invisible to every consumer.

E does not implement Watch. Missing Watch scenarios are recorded as unavailable
capabilities, never zero-cost baseline samples. Paired deltas are calculated only
for scenarios observed on both subjects. Candidate-only values remain useful
qualification evidence without a fabricated before/after speed claim.

Use `--smoke` locally for2-second fixture qualification only; these windows do not
satisfy long-window P2/P5 requirements. The full run is restricted to manual CI.
Native Windows ConPTY measurement is unavailable and recorded explicitly; an
ordinary Windows functional CI success does not replace that observation.
All data is synthetic, endpoints are loopback mocks, terminal capture is bounded,
and the fixture verifies owner/shell cleanup. Source patches, logs and invalid
results are retained for diagnosis; there are no automatic measurement retries.


For a Watch clock/render follow-up, `native_tui_profile=watch` keeps only the
actual600s P2-Watch and60s P5-Watch windows per capable subject. It retains the
same source/build/binary/ACK/producer witnesses and same-runner before/after
comparison; it does not shorten formal windows or make a missing Watch baseline
zero-cost. Default `all` is unchanged. Pair/capability records identify the
profile. Use exact baseline/candidate refs and review actual draw timestamps,
bins, CPU and UI read counters; green fixture validation is not a performance
threshold waiver. Windows still records unavailable, never measurements.
