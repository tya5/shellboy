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

`thread delete-preview`（wire `thread.delete_preview`）を加え、現在の公開操作は35。ThreadDeleteArgsの必須Threadとdescendantsだけを用い、読み取りで削除確認やGCを始めない。F8は既存indexを維持して末尾index 34に「Thread · 削除影響」を追加する。contextとoperation.get追加fieldはCLI出力の構造化resultへそのまま公開する。API契約は[全体設計](observability-and-deletion.md)と[protocol](protocol.md)を参照。

## 入力境界と起動経路の整理（R3-8、R3-9）

slash全体はUI状態操作を含むため表駆動化しない。help/readoutの初期表示（page・scroll・visible）はhelp moduleへ、空のThreadSetArgs/IndexArgsは型のDefaultへ寄せる。Defaultは既存のNone/空Vecを表し、serdeの省略・validationや索引対象の明示要件を変えない。補完候補・help（別名を含む）・実slash dispatchの一致は操作回帰で確認する。

入力pathは共通の「絶対ならそのまま、相対なら指定cwdへjoin」とUTF-8可否だけを共有し、解釈責務は混ぜない。CLI thread create/fork/setのcwd文字列は従来通りownerで解釈する相対値を保持し、CLI attachmentだけ既存の字句正規化を行う。TUI slash/formはThread cwdから解決する。symlink解決・存在検査やcwd全般の字句正規化は追加しない。TUIの操作pathが非UTF-8になる場合は、local draft変更またはwire生成より前に明示拒否する。表示用display()の置換文字と、操作先を変えてしまうto_string_lossyは区別する。非UTF-8の現在cwdでもUTF-8の絶対path指定は可能。

mainはstandalone JSON応答・owner RPC・TUI寿命を局所関数へ分ける。docs/ragへのoperation-id拒否、cwd取得、TUIのterminal確認→非同期起動進捗→取消し時owner close→bridge起動→終了drainの順序は維持する。standaloneの種類は既存CliCommandのmatchで分岐し、expect/unreachableを置かない。応答protocol_versionは既存PROTOCOL_VERSIONを正本とし公開JSONを変えない。詳細fieldから参照種別への対応はprivate UI TargetKindに寄せ、detailsの取得操作選択と操作フォームの参照context生成の双方で使う。source_node_id等の原文導線、currentと選択参照の区別は既存の別責務として保つ。新registry・公開wire optional分割は導入しない。


範囲編集は `shellboy compact --spill|--summary|--omit --from NODE --to NODE [--thread ID]`、先頭省略は `--trim --to NODE`、復元は `--restore [--from MARKER]` とする。モードは排他的。元の引数なしcompactを保持する。wireの`context.edit`は既存compactの受付/安全境界/receipt/downstreamへ接続する。[圧縮設計](compaction.md)を参照。
