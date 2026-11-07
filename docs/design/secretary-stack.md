# Secretary Stack: Generic Lower Layers and Where They Live (Memo)

Status: **decision memo**. The decisions in section 2 were made by the user on
2026-11-07. It adopts no new mechanism: each part keeps its own issue and design.
Statements about what exists were read from docs and code at `origin/main`
`5e140d3011`. **Nothing was run for this memo.** A statement here is not a
verification; see [acceptance](../validation/acceptance.md).

Goal: an AI secretary on the Shellboy the user runs at work (Windows, TUI today, Web
UI expected to become main, a resident server on 127.0.0.1 acceptable). Scheduled
triggers and cooperating Threads do mail intake and distribution, minutes and chat
intake, task and Thread generation, periodic Thread cleanup, and knowledge
accumulation.

Boundary: the secretary is **not** bundled into Shellboy or the Web UI. App-specific
top layers are set up on site by Shellboy itself. This memo fixes only the generic
lower stack; section 6 is the outline for the on-site part.

## 1. Stack and placement

| Need | Lives in | Mechanism | State on `origin/main` |
|---|---|---|---|
| Time-based trigger | External app | Windows Task Scheduler first, Dagu later | Nothing in core, by decision |
| Mail, chat, minutes connectors | On-site top layer | Scripts written on site | Not Shellboy's |
| Processed-ID ledger | On-site top layer | A file | Not Shellboy's |
| Job entry | CLI | `exec`, `thread create`, `message send`, `wait`, `chat --json` | Exists |
| Running owner | Core | TUI, or `serve --data-dir DIR` | Exists; the CLI never starts one (section 3) |
| Structured handoff | CLI | `exec --schema` | Exists, with a limited validator (section 4, R1) |
| Change following | Core + CLI | `events.wait` / `events wait` | Exists (#243) |
| Stable external contract | Core + docs | Protocol policy, `shellboy protocol schema` | Only unknown-field replies (#244 A1) |
| Model asks a human | Core + CLI + TUI | Reply-expected to a human (#262) | Not on main |
| No caller-based refusals | Core | #256, #259 | Stop, retry, shell done; rest not on main |
| Disk guard | Core | Capacity gate plus volume floor (#261) | Logical limit only |
| Thread cleanup | Core + CLI | `thread archive` (#222 stage 1) | Exists |
| Tags, re-parent | Core + CLI | #222 stages 2-3 | Not started |
| Knowledge | CLI + files | `rag`, `history search`, plain files | Exists |
| Cheaper index | Core | #253, #254 | Not on main |
| Web UI | Bridge (separate repo `shellboy-studio`) | Local IPC to the owner; HTTP/WebSocket to the browser on localhost | Not usable yet per its README |
| TUI on a running owner | Core | #247 | Study only; not a prerequisite |
| Spend limit | External | Provider or LiteLLM budget | No spend limit in Shellboy |
| Liveness, failure alerts | External | Scheduler history, on-site monitor | Not Shellboy's |
| Operating recipe | Docs | Section 6; a bundled guide later (section 7) | This memo |

The owner has no TCP listener: its IPC is a Unix socket or a named pipe
(`src/ipc/server.rs`). A 127.0.0.1 listener belongs to the bridge, not core.

## 2. Decisions (user, 2026-11-07)

| # | Decision | Consequence |
|---|---|---|
| D1 | Scheduling is in neither core nor the gateway. External scheduler: Windows Task Scheduler first, Dagu later if a list and history are wanted. They call the `shellboy` CLI; no product-specific adapter | No scheduler table, worker or setting in core |
| D2 | No automatic mail sending. The secretary creates drafts, the human sends from their mail client, it gets no send credential | Limit in section 4, R3 |
| D3 | A model asks a human through #262: reply-expected messages addressed to a human, because set-and-clear tags leave a cleanup problem | Questions can accumulate. No automatic expiry |
| D4 | TUI shows questions in a region under the common region spec, scoped to the displayed Thread only; the Thread list shows a pending count. Answer via CLI `message reply` or TUI | No global cross-Thread list. A Web inbox is built from the per-Thread counts |
| D5 | No caller-based authority: stop, retry, shell operations, message cancel, reply and delete do not refuse based on who calls (#256, #259). Origin only records who acted | State-integrity conditions stay (below) |
| D6 | Disk guard #261: hold new work when volume free space falls below a floor (default 5 GiB, settable); logical limit default 50 GiB | Commands a model's shell is already running are not stopped |
| D7 | Index: API-free int8 (#253); auto-index targets default to user messages and summaries (#254) | Tool output is indexed only by explicit request |
| D8 | TUI stays available as the light option. #247 is not a prerequisite; re-prioritize if Web memory measurement says so | One owner per data directory until then (section 3) |

State-integrity conditions that stay under D5: delete refused with a live or stopping
Run; retry by generation CAS; one reply per message; reply only after application; the
128 pending reply cap; one owner per live Run.

### Issue status on `origin/main`

| Issue | On main | Not on main |
|---|---|---|
| #222 | Stage 1: `thread archive`, `thread unarchive`, `thread list --archived`, TUI keys | Re-parent, tags, cwd grouping. Pin dropped |
| #243 | Stages B, C: `events wait`, chat on hints | Live text deltas (E3) |
| #244 | A1: typed `invalid_argument` for unknown commands and fields | A2 (protocol 3, capabilities), D (`protocol schema`). [Its design](events-and-external-protocol.md) requires both before `shellboy-studio` depends on the RPC |
| #247 | Nothing. `serve` guide: "A TUI cannot be attached later" | All |
| #253 | Nothing; no int8 code | All |
| #254 | Only the on/off switch `indexing.auto_history` | Target selection and its default |
| #256 (closed) | Stop, retry and shell write/EOF/stop have no authority check | |
| #259 | Nothing. Still present: `not_recipient` (`src/control/reply.rs`), `Forbidden` on cancel (`src/control/pending_messages.rs`), no reply-expected to own Thread and "owned by another live parent Run" (`src/control/db.rs`), exec 128 KiB (`src/exec_client.rs`) | All. The parent-send item follows #222 stage 2 |
| #261 | Logical limit only, defaults 8 GiB warning and 10 GiB stop (`src/config.rs`, `require_capacity_tx` in `src/gc.rs`). `storage status` already reports `filesystem_free_bytes` | Floor, new defaults, 2x-floor warning |
| #262 | Nothing. Reused parts exist: `--expect-reply`, `message reply`, `wait reply_received`, TUI `/replies`, `RegionLayout` (`src/ui/region.rs`) | All. Follows #259 |

## 3. Owner modes

Conversation commands need a running owner and never start one. An already-owned data
directory is an error, not an attach. So a data directory has one owner at a time.

| Owner | Scheduled job | TUI region (D4) | Answer a question | Note |
|---|---|---|---|---|
| TUI | Works only while the TUI is open; otherwise exit 6 from `exec` | Yes | TUI or CLI | Lightest; no job runs while closed |
| `serve` | Works while `serve` runs | No TUI on this data directory until #247 | CLI, or Web | `serve` does not daemonize; the scheduler or a login item must keep it alive. `shutdown --data-dir DIR` is the Windows stop path |
| `serve` + bridge | Same | Same | CLI; Web once it exposes questions | Bridge is a client of the owner |

Restart does not resume interrupted work. A Turn in `recovery_wait` waits for an
explicit decision; elapsed time alone does not resume it.

## 4. Open points and risks

| # | Point | State | Direction |
|---|---|---|---|
| R1 | Untrusted mail text reaches an agent with a shell (prompt injection) | **Open.** `exec` is "not a tool-free transform or a sandbox" and the shell is the model's only tool; the guides show no setting that removes it. Threads and worktrees do not isolate permissions | Isolate the intake worker: a dedicated Thread and cwd, the script (not the model) fetches mail, only structured fields go on through `exec --schema`. This limits what reaches the next Thread; it does not stop the intake worker's own shell |
| R1a | Schema strength | The validator supports type, properties, required, boolean `additionalProperties`, items, min/max items and length. `enum`, `const`, `pattern`, `format` are rejected before owner contact. Valid shape is not correct content | The job script checks allowed values and IDs itself. Treat free-text fields as data, not instructions |
| R1b | Blast radius under D5 | A hijacked worker's shell may stop, retry, or delete any eligible Thread, and has the owner's OS account. D5 removes no real boundary (the [safety model](../guides/user-guide.md#safety-model) already says Origin is none) but rules out a Thread-level fence | The boundary can only be the OS |
| R2 | Restricted second owner via `serve --data-dir` | **Unverified.** A second data directory under one account is no boundary: `--data-dir` reaches any owner the account can reach. It needs a separate account, which a company PC may not allow. The [Windows options](../guides/shell.md#windows-no-lightweight-equivalent-wrapper-supplied) are unverified too; `[shell]` wrappers apply to every Thread | Check on the real machine (section 5). If unavailable, accept and record the residual risk, keep the account's mail reach minimal |
| R3 | D2 is not enforced by Shellboy | Withholding a credential does not cover what the OS account already reaches. A signed-in mail client on that account may be drivable from the shell. Not checked | Check on the real machine. Keep any draft-scope credential with the job script, outside the owner's environment; shells inherit it |
| R4 | No spend limit in Shellboy | No price catalogue (effective spec). `usage get --group-by day` reports saved usage, not money. A provider spend-limit 429 fails until access resumes | Provider or LiteLLM budget first (not tried here). Add to the capacity gate only if needed |
| R5 | Long-lived secretary Thread versus a fresh Thread per job | **Cost unmeasured.** No figure assumed | Run both on one real job; compare `usage get --thread T` per landed item; keep the cheaper |
| R6 | Liveness and failure alerts | External | Scheduler history, exit codes, a heartbeat file (section 6) |
| R7 | Archive can hide a question | #262 unarchives when a question is created. An archive afterwards hides the Thread's pending count from the default list | Cleanup excludes Threads with a pending count. #262 should settle whether `thread archive` reports them |
| R8 | Question cap versus "can accumulate" | Today: 128 pending per recipient Thread. How to count for a human recipient is an open #262 item | Decide in #262 |
| R9 | Disk guard limit | A running command's own writes are not stopped. Free space that cannot be read does not hold work | Stated in #261; guide text with it |
| R10 | Web readiness | Bridge depends on #244 A2 and D, which are not on main | Keep TUI or CLI as the fallback |

## 5. Windows: check on a real machine

Windows is checked only by CI here (`windows-latest` test suite). None of this has
run on a physical machine or a company PC.

| Area | Check |
|---|---|
| Owner lifetime | `serve` from a scheduled task or login item stays up: task run-time limit, logoff, sleep, final `stopped` row after `shutdown --data-dir` |
| Job to owner | A CLI call from a scheduled task reaches the owner's named pipe: same account, and "run whether logged on or not" (another session) |
| Second account | Local policy allows it; data directory ACLs; whether the pipe (default security, `src/ipc/server.rs`) is reachable across accounts |
| Encoding and exit | PowerShell 5.1 and 7: UTF-8 `--text-stdin`, Japanese text, stdout and stderr separation, `$LASTEXITCODE` from a task |
| Scheduler | No overlap on long runs; missed-run behaviour; history visible |
| Mail reach | Whether the model's shell can drive the signed-in mail client or read saved tokens (R3) |
| Network | Company proxy to the provider or LiteLLM ([known limitation](../development/known-limitations.md)) |
| Disk | `filesystem_free_bytes` on the actual volume, including redirected folders; after #261, hold and explicit retry |
| TUI | Region keys and the archive keys in the real terminal; IME |
| Latency, memory | `events wait` over the pipe (not measured); owner, TUI and bridge memory (input to #247) |

## 6. Operating recipe outline (on-site, product-neutral)

For the on-site Shellboy writing the top layer. Try it first on a disposable
`--data-dir`. Replace names with returned IDs. Bash shown; PowerShell needs the
[encoding setup](../guides/cli-reference.md#noninteractive-work-with-a-json-result).

**Shape.** The scheduler runs a script. The script, not a model, holds source access,
the ledger and all routing. Models receive text and return JSON.

**1. Scheduled job.** One entry per job; forbid overlap in the scheduler and with a
lock file. Pass `--data-dir DIR` if the owner is not on the default directory. No owner
means exit 6: alert and leave items unprocessed.

**2. Ledger.** One append-only file per source, written by the script.

| State | Written | On the next run |
|---|---|---|
| `started <item-id> <thread-id>` | After `thread create`, before sending | Inspect `thread get --thread T`; do not resend |
| `done <item-id> <output-ref>` | After output is stored | Skip |
| `quarantined <item-id> <reason>` | Exit 3 or a failed script check | Skip; show a human |

`exec` rejects `--operation-id`, so create the Thread first and record it. That makes a
crash between steps inspectable rather than a duplicate. Use `--operation-id` on single
commands such as `message send`; resend with the same ID, never a new one.

**3. Intake with structured handoff.**

```sh
shellboy thread create --cwd "$INTAKE_DIR" --agents "$INTAKE_AGENTS"   # record thread_id
shellboy exec --thread "$T" --schema intake.schema.json --text-stdin \
  < item.txt > out.json 2> meta.json
```

```json
{"type":"object","additionalProperties":false,
 "required":["item_id","category","summary","needs_human"],
 "properties":{"item_id":{"type":"string","maxLength":200},
  "category":{"type":"string","maxLength":40},
  "summary":{"type":"string","maxLength":2000},
  "needs_human":{"type":"boolean"}}}
```

`$INTAKE_DIR` holds no secrets; `$INTAKE_AGENTS` says the input is data. Only the
Schema's fields go on, never the raw text. The script checks `item_id` against what it
fetched and `category` against its own list, since the Schema cannot. Input is limited
to 128 KiB today (#259 raises it); split or quarantine above that.

| `exec` exit | Meaning | Script does |
|---|---|---|
| 0 | Valid result | Check fields, store, `done` |
| 2 | Invalid input or unsupported Schema | Fix the job; alert |
| 3 | Mismatch after repairs | `quarantined` |
| 4 | Recovery, held, stopped, failed, unavailable | Leave `started`; alert; a human decides `run retry` or `run stop` |
| 5 | Timeout; **the work keeps running** | Leave `started`; inspect later; do not resend |
| 6 | Owner, transport or output failure | Alert; inspect saved targets before any resend |

**4. Distribution.** The script sends validated fields to a working Thread:

```sh
shellboy message send --thread "$S" --operation-id "job-$ITEM" \
  --sender-label scheduler --text-stdin < fields.json
```

Check `thread get --thread "$S"` first. A message from an external CLI **resumes** a
Thread in `recovery_wait`, like a human instruction (`src/control/db.rs`); do not let a
schedule do that unreviewed. If a human or model is using the Thread, add
`--deliver complete`.

**5. Draft-only output.** The model writes a draft file to an outbox. The script, or the
human, turns it into a draft in the mail system with draft-only access if the system
offers one. The human sends. No send step exists in the job. Record the draft reference
in the ledger.

**6. Asking a human.** Until #262: `needs_human: true` puts the item in a review file
and the job stops there for that item. After #262: the model sends a reply-expected
message to a human and ends its Turn, and the answer starts a new Run. Do not use tags
as a question flag.

**7. Cleanup by archive.** A periodic job, with IDs chosen by the script:

```sh
shellboy thread list --archived exclude --limit 200    # page with --cursor
shellboy thread archive --thread ID_1 --thread ID_2    # at most 200 IDs per call
```

Select by ledger membership, `state` and `updated_at`. A live Run does not block
archive, so exclude live Threads yourself, and those with a pending question (R7).
Nothing archives automatically. A new Run unarchives. Archive frees no storage:
`thread delete`, `thread prune` and `storage collect` are separate explicit steps.

**8. Knowledge.** Write what is worth keeping as files in a knowledge directory, then
`rag source add`, `rag index --source NAME --dry-run`, `rag index --source NAME`, and
`rag search --scope source:NAME`. Past work: `rag search --scope history --mode keyword`
or `history search`. Semantic indexing costs money; dry-run first.

**9. Health.** Each successful job updates a heartbeat file; an external check alerts
when it goes stale. A daily job can record `usage get --group-by day` and
`storage status`, and list Threads in `recovery_wait`, for a human to read.

## 7. Agent Guide

**No pointer added now.** The conventions do not call for one. `agent-guide.md` is
bundled and links no design document; a link there would come back from `docs get` as a
repository link with `available_offline: false` (`src/docs.rs`). Guides describe
implemented operations only ([guide design](guides.md)).

So the recipe above cannot be read by `shellboy docs` on a machine with only the
binary. Until the last row below lands, hand it to the on-site Shellboy as a file.

| When | Add to guides |
|---|---|
| #259 lands | Remove "a parent cannot reply on the child's behalf" and "cannot send a reply-expected message to its own Thread" (Agent Guide, [Reply to a request](../guides/agent-guide.md#reply-to-a-request)), same in `cli-reference.md` and `delegation.md` |
| #261 lands | [Storage and resources](../guides/agent-guide.md#storage-and-resources): floor, 50 GiB, running commands' writes not stopped. New defaults in `configuration.md`; hold and retry in `recovery-and-storage.md` |
| #262 lands | Agent Guide section "when a human decision is needed"; region and count in `tui.md` |
| #253, #254 land | History and index text: defaults, explicit backfill, `scope_status`, keyword fallback |
| #222 stages land | Tags in the cleanup step, Agent Guide organization text |
| After #259, #261, #262 | Move section 6 to a bundled guide (new `docs.rs` catalogue entry, `link_target`, `tests/docs_cli.rs`) and link it from the Agent Guide. Needs a build and CI, unlike this memo |

## 8. Stale text and tensions found (not changed here)

| Where | Says | Actual |
|---|---|---|
| `docs/design/effective-spec.md` (list of what is not adopted) | "web UI, archive" not adopted | Archive is on main; the Web UI is a separate adopted project |
| `docs/guides/cli-reference.md` (`message send`) | "Ordinary machine messages never silently resume recovery_wait" | An external CLI message does resume it (`src/control/db.rs`); only a model's is held |
| #262 issue body | `message list --awaiting-human` | Superseded by its later comment and D4 |
| `docs/design/README.md` | Reply-expected "adopted, not implemented" | Implemented |
| `docs/development/status.md` | #256 and others "on branch, CI pending" | On main |
| D5 in this memo | "Origin only records who acted" | Origin also grants Run ownership on a parent's send and holds a model's message to `recovery_wait`. Neither is a refusal, neither is in #256 or #259; recorded here as remaining behaviour |
