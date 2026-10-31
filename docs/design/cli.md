# User CLI syntax

採用状態: parser実装済み。CLIの公開構文と設定解決を具体化する。`src/cli.rs` のClap parserは公開操作を既存の `protocol::Command` へ変換する。TUIと同じ操作を使い、独自のshell start、daemon、headless製品コマンドを追加しない。現行の操作契約は [protocol](protocol.md) と [CLIガイド](../guides/cli-reference.md) を参照する。初期CLI資料は非同梱。

`Cli` はclone可能で、globalの `--data-dir PATH / --config PATH / --operation-id ID` をどのsubcommand階層でも受付する。無subcommandはTUI起動を表す。data-dir/configの基準パスと優先順位、接続CLIの設定非上書き、handshake後のoperation ID生成は起動/接続層が担当する。parserは設定やendpointへI/Oしない。`Cli::try_parse_from` を利用し、help/version表示は正常表示、操作のparse失敗は呼出し元がJSONのinvalid_argument/exit1へ変換する。Clapの自動exitは使用しない。

基本形:

```text
shellboy thread list [--limit 50] [--cursor TOKEN] [--query TEXT]
shellboy thread get [--thread ID]
shellboy thread create [--cwd PATH] [--connection PROFILE] [--model ID] [--effort TEXT] [--parent ID]
shellboy thread fork --thread ID --node ID [--cwd PATH]
shellboy thread delete-preview --thread ID [--descendants]
shellboy thread delete --thread ID [--descendants]
shellboy thread set [--thread ID] [--cwd PATH] [--connection PROFILE] [--model ID] [--effort TEXT]
shellboy message send [--thread ID] (--text TEXT | --text-stdin)
shellboy message get --message ID
shellboy operation get --operation ID
shellboy run (get|retry|stop) --run ID
shellboy revisit set [--run ID] (--text TEXT | --text-stdin)
shellboy revisit (get|cancel) [--run ID]
shellboy turn get --turn ID
shellboy wait --conditions-json JSON [--timeout-ms 30000]
shellboy history list --thread ID [--limit 50] [--cursor TOKEN]
shellboy history get --node ID [--limit-bytes 65536] [--cursor TOKEN] [--encoding UTF-8]
shellboy history search --query TEXT [--limit 10] [--thread ID] [--kind KIND]... [--exclude-kind KIND]... [--from RFC3339] [--to RFC3339]
shellboy history relations --node ID --relation (previous|next|sources|replacements) [--limit 50] [--cursor TOKEN]
shellboy shell get --execution ID
shellboy shell read --execution ID [--cursor TOKEN] [--encoding UTF-8] [--limit-bytes 65536]
shellboy shell write --execution ID (--text TEXT | --text-stdin) [--encoding UTF-8]
shellboy shell (eof|stop) --execution ID
shellboy attachment add [--thread ID] --path PATH --kind (image|file)
shellboy attach PATH [--thread ID] --kind (image|file)
shellboy attachment list [--thread ID]
shellboy attachment remove [--thread ID] --attachment ID
shellboy compact [--thread ID]
shellboy index (status|backfill|retry|rebuild) [--scope (threads|all)] [--space ID] [--thread ID]... [--source ID]...
shellboy storage (status|collect) [--category (raw|managed-images|history|index|all)]
```

`--thread` を省略可能な自己対象型は有効originからControlが解決する。history searchは例外で、省略時は全体検索とし、originの自己Threadを補わない。外部CLIに「最後に開いたThread」を補わない。fork/delete/history listの必須対象は既存公開型に合わせて必須とする。接続profileとmodel IDは独立指定。`--effort ''` は明示解除、未指定は継承/変更なし。過去索引を始めるbackfill/rebuildは明示対象を求めるprotocol検査を通す。

`into_command(self,startup_cwd)` は必要時だけTokio stdinを読む。`into_command_with_stdin(self,startup_cwd,&mut reader)` でテスト可能にする。textとtext-stdinは必須排他で、CLI stdinは常にUTF-8本文として読む。shell writeのencodingはその後shell runtimeがpipe送信前に変換する。改行/NULを含む本文はそのまま保持し、trimや改行追加をしない。空本文も許す。stdin/argv本文のparser側安全境界は8MiB、上限+1byteまで読んで超過を検出しinput_limitとする。切捨て・部分送信はしない。最終draft/stdin設定上限とJSONframe上限はさらに各層で検査する。stdin読取り失敗/不正UTF-8では本文をerrorへ複製しない。

添付pathは**CLIの起動cwd**基準で一度だけ絶対化し、`.`/`..` をlexicalに処理する。stat/realpath/symlink追跡/コピーは行わず、不在ファイルもパス受付の型変換を通す。送信準備時の存在検査は別責務。UTF-8にできないpathは明示invalid_argumentで、置換文字による別pathへ変更しない。Thread cwd指定は親/Thread基準でserverが解決するため、CLI cwdへ勝手に絶対化しない。

`tests/cli.rs` は全35操作のmapping、global配置/default/effort解除、aliasと不在path、stdinの正確な本文/排他/不正UTF-8/byte上限、未知flag/範囲/wait構文/明示index対象、help/version、Unix非UTF-8 pathをfixture化する。これはparserの検証であり、JSON出力/exit code/IPC実通信は起動・接続統合試験で確認する。

2026-10-24検証: repoのControl driverが並行実装中のため、最初のparser検証は一時Cargo harnessから実ファイル `src/cli.rs` / `src/protocol.rs` / `tests/cli.rs` を直接参照し、8件成功を確認した。repo本体の全体build受入とは区別する。driver統合後に標準チェックで同じテストを実行する。

Control driver統合後、2026-10-24 macOSのrepo本体で `cargo test --test cli --test protocol` を実行し、CLI 8件とprotocol 10件の成功を確認した。

Shell readの `--encoding` は任意で、未指定時はexecutionの保存済みoutput encodingを維持する。明示指定時だけ再decode設定を変更する（`ShellReadArgs.encoding: Option<String>`）。起動ツールのUTF-8既定で保存されたexecutionでは引数省略もUTF-8となる。


## 受付結果照会の引数ID

ガイドの実操作確認で `operation get --operation ID` の対象IDがglobalの `--operation-id` へ伝播し、同じIDで別要求を行ったと判定される不具合を確認した。公開flagは変更せず、照会対象だけClapの内部arg IDを `lookup_operation_id` へ分ける。照会自身のIDと対象IDは独立し、global未指定時は新しい照会operation IDを生成する。`tests/cli.rs::operation_lookup_target_is_independent_of_global_request_id` がglobal指定の有無と4配置を検証する。現行の受付・冪等性契約を満たす是正であり、操作仕様の変更ではない。

## 検索と直接関係の追加契約

ユーザー採用の最小拡張としてhistory.relationsを追加した時点では、公開操作は34となった。現行数は末尾の削除影響追加節を参照。検索filterの型・上限・期間境界とprovenanceは [protocol.md](protocol.md) と [検索・関係設計](search-and-relations.md) を参照。CLIは繰返しのkind/exclude-kindを配列へ変換し、日時文字列をprotocolでoffset付きRFC3339として検証する。関係種別はClapで4種類へ閉じ、未知値を拒否する。F8も同じCommandを組み立て、末尾（index 33）へ新規操作を追加して既存の操作indexを維持する。

## 削除影響と保存済みコンテキスト

`thread delete-preview`（wire `thread.delete_preview`）を加え、現在の公開操作は35。ThreadDeleteArgsの必須Threadとdescendantsだけを用い、読み取りで削除確認やGCを始めない。F8は既存indexを維持して末尾index 34に「会話 · 削除影響」を追加する。contextとoperation.get追加fieldはCLI出力の構造化resultへそのまま公開する。API契約は[全体設計](observability-and-deletion.md)と[protocol](protocol.md)を参照。

## 入力境界と起動経路の整理（R3-8、R3-9）

slash全体はUI状態操作を含むため表駆動化しない。help/readoutの初期表示（page・scroll・visible）はhelp moduleへ、空のThreadSetArgs/IndexArgsは型のDefaultへ寄せる。Defaultは既存のNone/空Vecを表し、serdeの省略・validationや索引対象の明示要件を変えない。補完候補・help（別名を含む）・実slash dispatchの一致は操作回帰で確認する。

入力pathは共通の「絶対ならそのまま、相対なら指定cwdへjoin」とUTF-8可否だけを共有し、解釈責務は混ぜない。CLI thread create/fork/setのcwd文字列は従来通りownerで解釈する相対値を保持し、CLI attachmentだけ既存の字句正規化を行う。TUI slash/formはThread cwdから解決する。symlink解決・存在検査やcwd全般の字句正規化は追加しない。TUIの操作pathが非UTF-8になる場合は、local draft変更またはwire生成より前に明示拒否する。表示用display()の置換文字と、操作先を変えてしまうto_string_lossyは区別する。非UTF-8の現在cwdでもUTF-8の絶対path指定は可能。

mainはstandalone JSON応答・owner RPC・TUI寿命を局所関数へ分ける。docs/ragへのoperation-id拒否、cwd取得、TUIのterminal確認→非同期起動進捗→取消し時owner close→bridge起動→終了drainの順序は維持する。standaloneの種類は既存CliCommandのmatchで分岐し、expect/unreachableを置かない。応答protocol_versionは既存PROTOCOL_VERSIONを正本とし公開JSONを変えない。詳細fieldから参照種別への対応はprivate UI TargetKindに寄せ、detailsの取得操作選択と操作フォームの参照context生成の双方で使う。source_node_id等の原文導線、currentと選択参照の区別は既存の別責務として保つ。新registry・公開wire optional分割は導入しない。


範囲編集は `shellboy compact --spill|--summary|--omit --from NODE --to NODE [--thread ID]`、先頭省略は `--trim --to NODE`、復元は `--restore [--from MARKER]` とする。モードは排他的。元の引数なしcompactを保持する。wireの`context.edit`は既存compactの受付/安全境界/receipt/downstreamへ接続する。[圧縮設計](compaction.md)を参照。

## Revisit (#36)

The three additive protocol-v2 commands use optional Run targets. Omission is
resolved only by Control from authenticated shell Origin; humans must name the
Run. Set/cancel use ordinary mutation receipts and serialized-command
fingerprints; get is read-only and receipt-free. Set accepts nonblank text of
at most 8192 UTF-8 bytes, checked in both CLI and wire validation. Its stdin
reader consumes at most 8193 bytes and rejects overflow without truncation.
See [Revisit](revisit.md) for ownership, lifecycle and replacement semantics.


## Explicit index operations (#42/#55)

`shellboy index preview` without a selection previews failed authorized work
for global `index retry`. Backfill and rebuild still require an explicit source
selection. `shellboy index rebuild --scope all --dimensions N` creates an
explicit-dimension candidate and validates its first real input once before
continuing. It never falls back to a different N and preserves the old active
space on failure. Omitting N preserves the matching current active request mode.

`shellboy index probe [--dimensions N]` sends one fixed short input exactly once
and saves its observation. Both operations can incur API charges; a probe is
not permission for a subsequent rebuild. N must be in 1..8192; endpoint support
and retrieval quality are not inferred from a model alias. Status reports saved
space dimensions, segment counts, float32 payload bytes and bounded probe facts.
See [index operations](index-operations.md).

## Explicit instruction references (#51)

The [per-Thread instruction reference contract](agents-reference.md) supersedes
previous fixed-reference wording: explicit create/set/fork selection is allowed,
while cwd changes alone never rediscover instructions and same-Turn retry
continues using its durable snapshot.


## Pending Input (#52)

`message send` queues ordinary input. `message send --now` explicitly releases a
foreground shell wait; it does not stop the process or cancel response generation.
`message cancel --message ID [--message ID ...]` uses fixed IDs (maximum128) and
returns per-message outcomes and restore archive references in the standard JSON
envelope. It never means all messages arriving before execution. See the
[pending-input contract](pending-messages.md) and protocol for authority/replay.
No CLI Turn-interrupt option is introduced; TUI Esc and full Run stop remain
distinct operations.

## Standalone configuration commands (#69)

`config init` and `config check` are dispatched before owner RPC conversion and
reject `--operation-id`, like other standalone command families. Init uses
exclusive file creation of the bundled full template and returns `created=false`
without changing an existing path. Check uses strict loading, returns structured
path/missing-credential-name facts, and only GETs the existing bounded model
catalog. It never starts an owner, opens a conversation database or calls
Responses/embeddings. Native subprocess tests isolate HOME/APPDATA/XDG paths and
exercise only loopback mocks. See [configuration](config.md#first-run-setup-69).

## Noninteractive saved work: exec (#65)

`exec --schema FILE [--thread ID] [TEXT | --text-stdin]` is a thin owner client:
create a saved Thread by default, send a user request, wait for its immutable
assigned Turn, read its saved final answer, and validate locally. New Threads
accept the existing thread-create options; default cwd is the caller's cwd and
Threads/history are not automatically deleted. Existing --thread conflicts with
new-Thread options. No table, worker, queue, scheduler, provider request format,
AGENTS change or Core execution path is added. `run` remains lifetime management;
`exec` means noninteractive work, not a tool-free ask or a raw shell command.

The default unverified sender label is `shellboy exec`; existing Origin scoping
and permissions remain authoritative. --operation-id is rejected because the
client performs several RPCs. It probes through an existing read RPC to learn
the current owner instance, then allocates and reports known per-step operation
IDs. Never automatically resubmit a transport-uncertain mutation.

Append the final JSON contract and compact Schema to the user text. Default
--max-repairs=2 (allowed0..5) adds corrective user messages in the same Thread,
not invisible HTTP retries; they are saved and may incur ordinary model/tool
costs. Ask corrections not to repeat completed side effects. Existing active
Runs can accept queued input; never return an answer unless the message was
applied and its assigned Turn completed with an answer. Held/unassigned,
recovery_wait, stopped/failed or missing answers are explicit execution failures.
A dedicated new Thread is the safe default for concurrent automation.

stdout contains one validated JSON value and newline only. A JSON diagnostic on
stderr carries known Thread/message/Run/Turn/answer IDs, per-step operation IDs,
and repair count. Failure stdout is empty. Exit codes:0 success;2 input/unsupported
Schema;3 invalid JSON/Schema after the repair cap (raw answer and errors on stderr);
4 recovery/held/stopped/failed/no usable answer;5 overall execution timeout;
6 owner/transport/protocol/output failure. Timeout never sends stop/retry or
starts an owner. Known IDs permit inspection; an uncertain send must be inspected
by operation ID before resubmission. The timeout begins after input/Schema reads,
covers all RPCs/repairs and defaults300000ms (1..86400000); owner waits are bounded
chunks of at most30000ms under that monotonic deadline.

First-version Schema validation uses serde_json only, not a new dependency. The
accepted bounded subset is boolean schemas and object schemas using `$schema`
(2020-12 or draft-07 URI), title/description, a single type from object/array/
string/integer/number/boolean/null, properties, required, boolean additionalProperties,
single-schema items, minItems/maxItems and minLength/maxLength. Counts are literal
nonnegative integers<=1000000; length counts Unicode scalar values. Type-specific
keywords apply only to matching JSON instance types. All other keywords, union
types/tuple items/schema-valued additionalProperties, including $ref/$defs,
combinators, format, pattern and numeric constraints are rejected before owner
contact. This is not general JSON Schema draft conformance. Bounds: Schema64KiB,
32 Schema levels,2048 Schema nodes, composed user input128KiB, history payload4MiB,
final answer1MiB,100000 validation steps and8 bounded diagnostics. Exceeding a bound
is explicit failure, never silently accepting or truncating a validated result.


Final-answer retrieval needs one additive read projection: existing history.get
normally flattens reasoning and natural text together. Optional final_answer_only
(default false and omitted on ordinary serialized requests) selects the last
final_answer assistant message, or the last unphased assistant message if no
final phase exists. Commentary/reasoning are excluded. CLI history get exposes
--final-answer-only for exact diagnosis. Normal text projection and cursor scope
remain unchanged; the final projection has its own cursor scope and node/revision
binding. This does not add a Core execution path or change model output storage.

A bounded local index over serde_json-validated scalar spans preserves numeric lexemes. Integer
checks compare decimal fraction/exponent/trailing zeros exactly, without f64
rounding or a new crate; numeric spellings remain intact in stdout. The validation-step cap must report failure before any
unchecked tail can be accepted. Repair limits count corrective messages, not all
ordinary Core requests/tools/retries, and timeout can leave billable work running.


Diagnostic turn_usage is a bounded list (at most six completed answer Turns) of
existing Turn usage facts, not a per-exec total or a bill; unknown stays null.
An already active Turn may include other input. Completion means its final answer,
not necessarily Run/background-work termination. Oversize/pagination failures are
output failures (exit6) with answer IDs, not schema-cap failures missing a raw answer.


Return the validated raw JSON spelling (outer whitespace trimmed), not a serde
Value reserialization, so large decimal number spellings are not rounded by the
client. Parsing remains bounded and type checks enforce the documented numeric
subset. Model-result validity is shape evidence only, not factual correctness.


JSON instance indexing is limited to100000 nodes/depth128. Numeric parsing uses
the existing serde_json supported range; out-of-range numeric inputs fail explicitly.


### exec owner-version guard

Before Thread creation/message send, exec makes a read-only final_answer_only
probe with a random missing node. A current owner decodes the argument and reports
target_unavailable; old-style argument rejection exits6 before work. A refusal
at answer read also exits6 (owner_version_mismatch), retaining known answer IDs
and recommending ordinary history get --node ANSWER_NODE_ID without the new flag.
Do not resend completed work as if this were an input/schema error. Closed-wire
support failures are owner/capability errors6, not input2. This is detection only,
not an old-Core format shim or automatic answer/protocol downgrade.

## Bounded range-summary overrides (#73)

Only `compact --summary` accepts `--max-chars N` / `--max-percent P`.
They map to optional `ContextEditArgs.max_chars` / `max_percent`; owner-side
validation rejects nonpositive characters, nonfinite/out-of-range percentages
and limits on non-summary actions. Existing safety-boundary queue, operation
receipts and immutable source adoption are unchanged. See the bounded-summary
contract in [compaction](compaction.md).

## Saved readback queries (#75)

`compact list --thread T [--since TIME --until TIME] [--limit N]` reads saved
compaction operations, automatic-failure observations and retained replacements,
newest first. Unknown legacy metrics remain null and retention gaps are explicit.
`thread get` includes its latest retained compaction observation.

`usage get` accepts a half-open RFC3339 interval [since,until), selecting model
attempts by started_at, shells by started_at and Runs by created_at. Selected
lifetimes are counted whole, not prorated; unknown start times are excluded only
when a period is requested. Existing usage state/unknown/overflow rules remain.

`shell list` defaults to all conversations, with optional thread, period, command
substring (literal/case-sensitive), native state and exit-code filters. Maximum
page size is 200. A bounded candidate window permits empty filtered pages with a
continuation cursor; follow it until absent. Cursor scope/filter and insertion
watermark are bound, while each page reads current saved metadata. No output body
is returned. Snippets are bounded to 512 characters, stripped of controls and
conservatively suppressed for obvious credential shapes; this is not a guarantee
against arbitrary obfuscated secrets. Suppression includes secret-access-key flags,
sshpass password arguments, attached -p values and *_KEY/*_TOKEN/*_SECRET assignments.
Explicit shell get/read remain separate.

These operations run on read-only WAL snapshots through the existing finite
presentation admission, not the writer's queue. No new tables, workers or queues.
Derived indexes and their measured sizes are documented in acceptance. Query-plan
tests must seek persistent tables; ephemeral materialized/JSON iterators may scan.

## On-demand owner memory (#80)

`status --memory` is an owner-wide read-only observation, exclusive with activity
filters. It reports existing body/layout/attachment/replay cache accounting
(retained bytes, configured limits and existing entry counts), not a new heap
estimator. Protected working bodies and live/pinned shared allocations remain
separate from budgeted retention. Cache values and OS metrics are not additive.

Only this request collects memory facts. UI-owned counters travel through existing
UI event/update delivery, with one request-completion responder; no new work queue,
worker, sampler or periodic log is introduced. UI only reads its existing bounded
cache accounting; native process sampling and other cache stats run outside the
DB/UI lane through existing finite control-file admission. Missing UI, unavailable
native metrics and unsupported OSes are explicit null/unknown, never guessed.
macOS uses proc_pid_rusage footprint/resident bytes, Linux reads RSS/PSS from proc,
and Windows uses current-process working set/private usage. No cache caps change.

F8 presentation is deferred to overlapping #41. This first version does not
investigate leaks, optimize request-copy peaks or collect per-allocation data.

## Headless owner startup (#5 stage 2)

`serve --data-dir DIR [--config FILE]` starts the shared owner session without
terminal setup or a UI frontend. An explicit nonempty data directory is required;
`--operation-id` is not an owner-startup option. Default config lookup is unchanged.
The owner lock rejects an already-owned directory instead of attaching to it.
One flushed stdout JSON line with event/state `ready`, absolute `data_dir`,
`instance_id` and `protocol_version` follows IPC binding and dispatcher startup.
Startup errors are JSON on stderr, with no stdout result. stdin is not read and
EOF does not request shutdown. No daemonization, fork, setsid or breakaway is added.
Shutdown RPC, Unix signal handling and the normal final line are stage 3 work.

Startup never calls the CLI endpoint/origin lookup: the required explicit path
wins over inherited SHELLBOY_DATA_DIR, and inherited socket/instance/origin values
are not owner identity. Existing driver environment assembly issues child-owner
identity for each shell. API-key and proxy environment variables remain intact;
no runtime global environment mutation is needed. This stage adds no table,
worker, queue or UI adapter. Core review is required before integration.

Owner replacement/reload does not transfer running work in this feature. Keep
#82 ownership cleanup; no exec handover, UI reconnect or late UI attach is added.
