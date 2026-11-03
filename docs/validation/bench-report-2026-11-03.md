# Benchmark report: prompt-cache fix (#183), 2026-11-03

This report compares Shellboy **before and after #183** with the same harness, model and route, run on the same day and back to back. That pair is the primary comparison. Rows for other agents are the **2026-11-02 #151 measurements**, reused unchanged. They were not re-measured, so any cross-agent comparison spans different dates (see [Caveats](#caveats)). The fake-model resource methods are described in the [resource usage guide](../guides/resource-usage.md).

## Key findings

Each finding cites the tables below. All paid runs are N=1. Treat them as observations, not statistics.

1. **Before #183, consecutive Shellboy requests always diverged one item before the end of the previous request. After #183, every consecutive request strictly appends.** The paid run recorded 0/9 strict-append pairs for main `c090fa79` and 9/9 for `2669c1ba`. The fake-model preflights gave 0/8 and 8/8 ([Paid real-model runs](#paid-real-model-runs)).
2. **The provider cache hit rate on input roughly doubled.** Cached/input excluding the first request rose from 42.8% to 79.4% in the back-to-back pair. An earlier independent pair the same day, using debug builds, rose from 49.8% to 84.3%.
3. **Input billed at the uncached rate fell 55%, from 25,657 to 11,524 tokens.** The reference cost at terra prices fell 48%, from $0.0277 to $0.0144. Total input rose 5% (42,681 to 44,804), because earlier Turns' context lines now stay in history.
4. **Against the 2026-11-02 rows for other agents, Shellboy after #183 sent the least total input of the agents measured.** It sent 44.8K tokens; the others sent 95K to 113K. Its cache rate (74%) is now in the range between Codex/jcode (44–49%) and OpenCode (90%). On the reference cost, Shellboy after #183 ($0.0144) is below OpenCode ($0.0162), Codex ($0.0683) and jcode ($0.0603) for this synthetic workload. This spans different days and backend cache state, and it is not a product ranking.
5. **Free resource observations show no meaningful change from #183** in memory, startup or storage. Request bytes per conversation grew by about 7% (85,130 to 91,162 bytes at N=1), which is consistent with the retained context lines ([Free resource observations](#free-resource-observations-fake-model)).

## Cause and fix

Before #183, Shellboy rebuilt a per-request **context line** for each request, giving Thread, Run, Turn and current state. The line was always placed as the **last input item**. Because it was regenerated and moved, request *k+1* was not an extension of request *k*. The longest common prefix always ended just before the previous context line, at the previous item count minus one. Provider prompt caches match on exact prefixes. Every request therefore reused only the cached system/tools/early-history prefix, in 1,792- or 4,480-token blocks, never the growing conversation.

#183 saves the Turn's context line into history once, at the Turn start, and appends later changes as new items. Each request is now the previous request plus new items, so the whole previous request is a reusable prefix. Replacements such as summaries or spills re-add the current context right after the replaced range, so the context stays present after compaction. The cost is the extra tokens of older Turns' context lines that stay in history: about 2.1K extra input tokens over this workload.

## Conditions

| Item | Value |
| --- | --- |
| Date | 2026-11-03 (UTC). Paid runs 00:56:47–00:57:31 UTC, back to back: main, then #183 |
| Before | `c090fa799f1c5d873a36ea07994c936244377437` (main just before #183), release build, `shellboy 0.6.1`, binary SHA-256 `e4450812d8f0f8373c0ac9870c19d447fdc7c611223a0c74036fdf6cc0e4032d` |
| After | `2669c1ba59eadce04a1b800bc12e56c34606ae4c` (#183 on main, code commit `642fb0e9`), release build, `shellboy 0.6.1`, binary SHA-256 `7eb689f4c7f8da91a0eba1a538b150105d0333914d91cb790ee788f19f44cf82`. Before and after differ only by #183 |
| Model | `gpt-6.1-terra`, the model used in #151 |
| Route | Local LiteLLM on loopback, then the ChatGPT subscription backend. This is the same `litellm` connection used in #151. Responses API, streaming |
| Harness | #151 `scripts/bench` (`meter_relay`, `usage_budget`, `usage_trial.shellboy`) from branch `issue-151/real-usage` lineage, macOS `sandbox-exec` isolation, synthetic workspace, the same 5 instructions, output cap 2,048 |
| Run-specific limits | At most 20 requests per run, stop at the first provider error, no client or relay retries (`response_max_retries=0`). A thin driver also captured each request's item list in memory to judge strict append. It recorded only counts and booleans, not request bodies |
| N | 1 per build |
| Machine | macOS 26.3.1 arm64 (development Mac) |

Credentials were read from the environment by the relay only, and were never logged or stored. Temporary data dirs were deleted. The user's Shellboy data dir was not used.

## Paid real-model runs

### Per request (input, cached, output)

| # | main `c090fa79` | #183 `2669c1ba` |
| ---: | --- | --- |
| 1 | 2919, 0, 39 | 2907, 0, 32 |
| 2 | 3221, 1792, 5 | 3181, 0, 5 |
| 3 | 3521, 1792, 35 | 3618, 3072, 35 |
| 4 | 3791, 0, 15 | 3892, 2816, 15 |
| 5 | 4131, 1792, 38 | 4351, 3456, 47 |
| 6 | 4426, 1792, 44 | 4666, 3712, 54 |
| 7 | 4726, 1792, 7 | 4985, 4608, 8 |
| 8 | 5031, 1792, 29 | 5418, 4864, 29 |
| 9 | 5300, 1792, 13 | 5680, 5248, 7 |
| 10 | 5615, 4480, 5 | 6106, 5504, 5 |

Strict append compares each request's item list with the previous one. It also requires the non-history request fields (instructions, tools, settings) to be identical.

| | Pairs strictly appending | Common prefix |
| --- | --- | --- |
| main | 0 / 9 | Always the previous item count − 1 (e.g. 7→9 items share 6) |
| #183 | 9 / 9 | Always the full previous request, including pairs across Turn boundaries |

### Totals

Reference cost uses terra prices: $1 per 1M uncached input, $0.05 per 1M cached input and $5 per 1M output, computed as (input − cached)×1 + cached×0.05 + output×5 per 1M. **It is a reference only.** The subscription route is not billed per token.

| Run | Requests | Input | Cached | Uncached | Output | Cached/input, all | Excluding first request | Reference cost |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| main `c090fa79` (00:56 UTC) | 10 | 42,681 | 17,024 | 25,657 | 230 | 39.89% | 42.81% (9) | $0.0277 |
| #183 `2669c1ba` (00:57 UTC) | 10 | 44,804 | 33,280 | 11,524 | 237 | 74.28% | 79.43% (9) | $0.0144 |
| Earlier pair, main `c090fa79` (00:13 UTC, debug) | 10 | 42,521 | 19,712 | 22,809 | 214 | 46.36% | 49.76% (9) | $0.0249 |
| Earlier pair, #183 pre-rebase `d71c1e76` (00:13 UTC, debug) | 10 | 45,101 | 37,376 | 7,725 | 218 | 82.87% | 84.34% (9) | $0.0107 |

The earlier pair is the #183 verification measurement recorded in issue #183. It used the same harness and route, with a 15-request cap. Its branch build was `d71c1e76`, before the rebase onto `c090fa79`. Its first #183 request got 1,792 cached tokens, probably from the main run immediately before it. In today's final pair, the first two #183 requests had no cache hits.

`X-Shellboy-Session-Id` and `x-litellm-session-id` were present on 10/10 requests in both final runs. `x-litellm-trace-id` was present on 0/10. Header values were not recorded.

### Other agents (2026-11-02, #151, not re-measured)

The agent rows below come from #151, measured 2026-11-02 with the same harness, model, route, 5 instructions and N=1. The Shellboy 0.6.1 row is the historical pre-#183 measurement.

| Agent (date) | Requests | Input | Cached | Uncached | Output | Cached/input, all | Excluding first request | Reference cost |
| --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
| Shellboy 0.6.1 (2026-11-02, historical) | 10 | 42,433 | 17,920 | 24,513 | 224 | 42.23% | 45.35% | $0.0265 |
| Codex CLI 0.165.0 (2026-11-02) | 9 | 113,394 | 49,408 | 63,986 | 360 | 43.57% | 48.89% | $0.0683 |
| OpenCode 1.19.5 (2026-11-02) | 14 | 95,220 | 85,504 | 9,716 | 451 | 89.80% | 90.37% | $0.0162 |
| jcode v0.99.1 (2026-11-02, Linux container) | 10 | 110,794 | 54,272 | 56,522 | 216 | 48.98% | 54.21% | $0.0603 |
| **Shellboy main before #183 (2026-11-03)** | 10 | 42,681 | 17,024 | 25,657 | 230 | 39.89% | 42.81% | $0.0277 |
| **Shellboy after #183 (2026-11-03)** | 10 | 44,804 | 33,280 | 11,524 | 237 | 74.28% | 79.43% | $0.0144 |

Codex used the Responses API; OpenCode and jcode used Chat Completions (jcode in a disposable Linux container behind an internal-only network and a dual-homed metering relay). Claude Code was not measured, because its first request in #151 hit a provider error.

## Free resource observations (fake model)

These are the CI `footprint` and `compare` jobs, using `scripts/bench/footprint.py` and `compare.py` unchanged. Both run on GitHub `ubuntu-latest` against the deterministic loopback model, with no provider. Only the Shellboy rows are reported. The compare job also ran the other agents, but this report does not use those rows.

| Build | Workflow run | Footprint runner CPU | Compare runner CPU |
| --- | --- | --- | --- |
| main `c090fa79` | [41552141436](https://github.com/tya5/shellboy/actions/runs/41552141436) | AMD EPYC 7763 | AMD EPYC 7763 |
| #183 `2669c1ba` | [41553439861](https://github.com/tya5/shellboy/actions/runs/41553439861) | Intel Xeon 6975P-C | AMD EPYC 9V74 |

The jobs ran on different runners with different CPUs. Per the guide, measurements from separate jobs are not a paired comparison. Small memory and time differences below are within runner variation and are not attributed to #183. In run 41552141436 the compare job ended `error`, because Codex per-session process-tree churn hit the known #151 rebuttal 7 issue. The Shellboy cases in that run were `complete`. Run 41553439861 completed for all agents.

### Memory: Stage 1, Shellboy alone, PSS KiB (baseline → round 5)

| N | main | #183 |
| ---: | --- | --- |
| 1 | 21,306 → 27,373 | 21,373 → 27,516 |
| 5 | 21,445 → 28,680 | 21,400 → 31,863 |
| 10 | 21,353 → 31,216 | 21,518 → 33,509 |
| 20 | 21,232 → 34,706 | 21,493 → 35,844 |

### Memory, request bytes and storage: Stage 2/4, compare job, Shellboy rows

| N | PSS after round 5, KiB (main / #183) | Request bytes per conversation, median (main / #183) | Last request bytes (main / #183) | Storage bytes (main / #183) | Files |
| ---: | --- | --- | --- | --- | ---: |
| 1 | 27,013 / 26,802 | 85,130 / 91,162 | 13,439 / 14,947 | 5,026,374 / 4,923,350 | 6 |
| 5 | 29,015 / 28,505 | 85,150 / 91,176 | 13,443 / 14,950 | 5,296,446 / 5,312,422 | 6 |
| 10 | 30,602 / 31,827 | 85,150 / 91,182 | 13,443 / 14,951 | 5,857,718 / 5,923,206 | 6 |
| 20 | 35,053 / 32,028 | 85,169.5 / 91,214.5 | 13,446 / 14,957.5 | 7,017,222 / 7,082,494 | 6 |

Both builds made 9 requests per conversation. Request bytes grew by about 7%, matching the retained context lines. Bytes are not tokens: the provider cache reduces billed cost, not bytes sent.

### Startup

| | main | #183 |
| --- | --- | --- |
| Stage 1 cold TUI, first completed frame / pasted echo (1 observation) | 314.9 / 396.8 ms | 313.5 / 368.6 ms |
| Compare, first visible, median [min, max], 5 launches | 11.035 [10.648, 12.379] ms | 9.889 [9.507, 10.638] ms |
| Compare, typed-probe echo, median [min, max] | 72.858 [72.740, 74.236] ms | 71.130 [70.617, 96.286] ms |

Startup includes owner startup for a fresh data dir. The echo uses the 50 ms retyped probe, so it is not a pure key-to-display time (see #151 rebuttals 8–9). The Stage 1 cold-frame figures are much higher than the 2026-11-01 guide figures (about 4.5 ms). That method and this run's data are recorded in the artifacts, but this report does not investigate the difference. #183 does not touch startup code.

## Caveats

- **N=1** per build for paid runs. One back-to-back pair plus one earlier same-day pair is two observations of the direction, not a distribution. No significance is claimed.
- **Backend cache variance.** Hit rates depend on provider-side cache state, timing and routing. For example, main's request 4 and #183's request 2 had no hits despite matching prefixes. Treat cache rates as observed, not guaranteed. A fresh conversation is not guaranteed cold, and a matching prefix is not guaranteed hot.
- **LiteLLM internal retries are unknown.** LiteLLM retries up to 2 times on errors by default. The relay saw no error responses, but it cannot see usage from internal retries.
- **Synthetic workload.** Five tiny instructions in a three-file workspace, with no reasoning-heavy work. Absolute token counts mostly reflect each agent's system prompt and tool schema size. **This is not a product ranking** and not a prediction for real sessions, where long histories make strict append matter more.
- **Different dates for other agents.** Codex, OpenCode and jcode were measured on 2026-11-02 and were not re-run, to avoid spending on unchanged agents. Cache state and backend behavior can differ between days.
- **Reference cost** applies terra list prices to reported tokens. The subscription route does not bill per token.
- **Fake-model resource runs** ran on different GitHub runners and CPUs for the two builds, so they are not a paired comparison.

## Reproduction notes

The paid driver wraps the #151 harness. It reuses `usage_trial.shellboy` unchanged, adds a 20-request cap through `Budget.admit`, and captures item lists in memory through `meter_relay.validate_request`. It is not committed. The harness modules are on branches `issue-151/real-usage` and `issue-151/jcode-retry` and are not on main. The free runs are reproducible with `gh workflow run ci.yml --ref <ref> -f footprint=true` and the artifacts named in the runs above.
