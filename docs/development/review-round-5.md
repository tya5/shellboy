# Additional #32 / #33 review follow-up

Baseline: `6c577c8`; review comments #32/6484080681 and #33/6484081072.
The prior four #32 findings and #33 Medium/eight Low findings are confirmed
resolved. This follow-up is integrated alongside issue #34, with separate local
commits and verification records. The implementation is pushed; issue closure
awaits the native verification gate.

| Finding | Disposition |
| --- | --- |
| #32 Low 1: closed autosave worker repeatedly emits critical failures | One local closed flag stops pumping the dead worker after the first failure. Retain backlog/newer snapshots and the shutdown failure; test 1,200 ticks emit exactly one notice. |
| #32 Low 2: Changed before a fresh pass causes an unnecessary repeat | Clear repeat when starting an entirely new scan, preserve it during partial scans. Test 1/128/200 receipts both initially and after a completed pass, and retain the mid-pass fairness regression. |
| #32 same-Thread closing sends | Existing selection-revision CAS conflicts are outside this delivery fix. Do not reinterpret independent-Thread persistence as a same-Thread guarantee; drafts remain retained on rejection. |
| #33 Low 1: any worker Err requires manual retry, diagnostic failure obscures the stopped index | List capacity, authorization, claim and configuration cases in design. Record one bounded in-memory stop cause, distinguish search error from owner shutdown, and surface it in status when DB reads remain available. |
| #33 Low 2: rag init does not report actual migration | Return schema_version and nullable migrated_from_schema; old stores report their actual previous version, fresh/current stores return null. Preserve source IDs/counts, do no indexing/API calls, and report readiness. |
| #33 response operation-ID mismatch wording | Keep conservative endpoint/instance diagnostic and uncertain mutation result for now; no false claim of non-application. This is nonblocking and the review requests no fix. |
| #33 mixed historical heading language | Keep prior Japanese archive and new English sections per the adopted language policy; no cosmetic translation sweep. |
| #33 prior test failure cause | Attribute the independent raw TCP and post-reboot evidence to the reviewer, reproduce current raw loopback success locally, and rerun exact baseline plus HTTP/PTY. Do not claim TCP_NODELAY fixed a kernel/network fault. |

## Evidence and remaining gates

Before current Rust checks, raw Python TCP on 127.0.0.1 delivered 1KiB, 4KiB,
16KiB, 100KB, 1MB and 10MB completely. The reviewer independently observed the
earlier lo0 16,332-byte stall and recovery after restarting the Mac. Its timing,
large-body boundary, unchanged-source failures and post-reboot recovery identify
the prior loopback fault; high load alone is not the explanation.

An isolated copy of exact `6c577c8` passes the standard check after restart
(76 groups / 1,343 passed / 0 failed / 24 ignored) and the original ten-conversation
HTTP+PTY script (all ten complete, exact output, zero external requests). The
isolated variant without all three `8368874` test-file changes also passes
582 library cases (0 failed, 10 ignored). Commit `2f8a198` removes that
unnecessary workaround; it was not a demonstrated fix for the loopback fault.

Final current source `d84a06f` passes format, strict all-target Clippy and
80 groups / 1,365 passed / 0 failed / 25 ignored. Current ten-conversation
HTTP+PTY also passes and independently confirms ten durable header observations.
The #32 fixes are `4c1d200`; #33 store/index fixes are in `cf1e7f2`. The user
subsequently authorized push; native three-OS CI remains pending. No release or
installed-binary update is included.

## Native findings and final local continuation

Windows FIFO fixtures now progress on known capacity rather than 512 timed
sleeps, retaining their five-second watchdog and all FIFO/count/permit checks.
The native Windows job passes on b938c68. The same run exposed a separate
macOS RAG registration busy result, now diagnosed and fixed in f659197 by
explicit writer-lease release. See decisions for evidence and unproven OS
details. Final local gates pass 80 groups / 1,366 tests / 0 failed / 25 ignored
and 280 parallel RAG cases. A new native three-OS gate remains pending.


Run [40802346281](https://github.com/tya5/shellboy/actions/runs/40802346281)
passes Linux and macOS, including the RAG writer-lease fix. Windows passes the
FIFO and RAG cases but exposes an owner-cleanup watchdog mismatch and two
unlocalized attachment incoming-request watchdogs. These are separately
tracked in acceptance; there is no final three-OS success yet.
