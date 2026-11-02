# Issue 151: startup probe timing investigation

The old `64.974 ms` result is process spawn to observed probe echo, not a
keystroke-to-echo latency. At the measured Linux SHA `5a2e8474`, the harness
writes at first visible text then every 50 ms, and all five Shellboy samples
had two writes. The startup screen discards ordinary typed keys and paste;
while initialization is pending, its event poll has a maximum 25 ms timeout.
The observable delay therefore contains process/owner initialization and probe
retry timing. The old artifact has no per-write timestamps or composer-ready
timestamp, so it cannot prove the exact contribution of either delay.

## Exploratory macOS observations

On macOS 26.3.1 arm64, source `4f2c237b0765ac332f0d2198e6bf6d59796af983`,
a local debug build and the same 100x36 PTY harness were run five times per
retry interval. Probe write timestamps were added to the harness. No probe was
submitted and the dummy endpoint was `127.0.0.1:9`; no inference was sent.
These are not the Linux release build or the original runner. All times are
milliseconds from process spawn, and samples are in execution order.

| Retry interval | First visible | Probe write times | Observed echo |
| ---: | ---: | --- | ---: |
| 10 | 2832.14 | 2832.17, 2850.49, 2861.86, 2874.41, 2885.33, 2899.57, 2912.11, 2924.66 | 2925.46 |
| 10 | 33.88 | 33.91, 52.80, 64.64, 78.33, 88.40, 100.94 | 105.05 |
| 10 | 34.71 | 34.74, 53.87, 66.09, 79.95, 92.56, 103.80 | 106.68 |
| 10 | 43.84 | 43.86, 61.81, 78.55, 88.60, 101.43, 112.86, 124.83 | 128.84 |
| 10 | 35.33 | 35.36, 55.09, 74.01, 85.45, 95.62, 114.12, 124.41, 134.66 | 141.11 |
| 50 | 39.25 | 39.28, 94.43 | 126.47 |
| 50 | 20.85 | 20.87, 79.61, 138.05 | 167.53 |
| 50 | 31.17 | 31.20, 87.75 | 116.21 |
| 50 | 28.94 | 28.97, 80.04, 139.41 | 165.27 |
| 50 | 21.20 | 21.22, 72.78 | 102.00 |
| 100 | 20.90 | 20.92, 123.99 | 137.46 |
| 100 | 20.66 | 20.68, 121.83 | 135.33 |
| 100 | 20.42 | 20.44, 120.99 | 134.71 |
| 100 | 28.75 | 28.77, 132.44 | 145.99 |
| 100 | 28.00 | 28.02, 135.75 | 149.20 |

The first visible output was a startup screen in all samples. The 10 ms group
contains a 2.8 s outlier before its first visible screen. Scheduling, run order,
startup variability and only five samples limit inference: these data do not
identify a single 65 ms internal timer or isolate pure key-to-paint latency.
A future test should identify individual probe writes, record composer readiness,
and measure one keystroke after readiness independently of launch time.

Reproduce on a disposable host with
`python3 scripts/bench/startup_interval.py --binary /path/to/shellboy --runs 5`.
