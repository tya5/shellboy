# ガイド体系の設計

2026-10-24。対象は現在のshellboy初版を使うためのガイド一式。仕様基準は [適用仕様](effective-spec.md)、公開操作は `src/cli.rs` / `src/protocol.rs`、画面操作は `src/ui.rs`、モデル入力は `src/control/driver/request.rs` と照合する。

## 読者と入口

人間はUser Guideから起動・会話・操作へ進む。shellboy内のモデルはAgent GuideからshellとCLIを使った実行手順へ進む。開発者向けの設計・検証・原文仕様は既存の配置を維持する。リポジトリ直下の `AGENTS.md` はshellboy自体の開発指示であり、Agent Guideと混同しない。

ガイドは `docs/guides/` の一階層へまとめる。読者ごとに同じ機能説明を複製せず、二つの入口から機能別文書を共有する。English is the default for unsuffixed user/feature guides and the English-only Agent Guide. Japanese user-facing guides are preserved as `.ja.md`; the [language policy](documentation-language.md) supersedes the original Japanese-only guide language choice. 短いsystemから詳細を参照する方針を保ち、巨大なプロンプトをガイドへ転記しない。

## 作成する文書と責任

| ファイル | 読者・内容 | 完成の条件 |
|---|---|---|
| `guides/README.md` | 全読者の案内・読む順序・用語 | 全ガイドへ到達でき、開発資料と区別できる |
| `guides/user-guide.md` | 人間の最初の利用 | 起動、送信、切替、委譲、終了、次の参照先が分かる |
| `guides/agent-guide.md` | shellboy内のモデルの操作手引き | 自分の文脈取得、shell/CLI、子作成→送信→割当→待機→原文取得を実行できる |
| `guides/configuration.md` | 導入・接続・設定 | OS別配置、優先順位、資格情報、TOML、変更範囲、主要予算を説明する |
| `guides/tui.md` | 人間の画面操作 | 実装済みキー・slash操作・overlay・下書き/貼付けを案内する |
| `guides/delegation.md` | 人間/モデルの子委譲と連携 | 子/独立Thread、所有、進捗、any待機、復旧、親終了時の違いを具体化する |
| `guides/shell.md` | モデルの外部作業・人間の観測 | foreground/background、stdin/EOF、出力/encoding、停止、shellツールとCLIを区別する |
| `guides/history-and-search.md` | 過去情報の取得・検索 | list/get/relations/fork/編集、compact、原文・出典、検索filter・索引・明示backfillを案内する |
| `guides/attachments.md` | ファイル/画像入力 | 外部参照とmanaged画像、追加/除外/更新/欠落、上限/モデル依存を説明する |
| `guides/recovery-and-storage.md` | 状態確認・復旧・運用 | 受付と完了、retry、通常/強制終了、保持/GC、代表errorへの対処を説明する |
| `guides/cli-reference.md` | 人間/モデルのコマンド辞書 | 全35操作、alias、共通接続/JSON/exit、wait/cursor/冪等性を参照できる |

User Guide → TUI/設定 → 必要な機能別文書。Agent Guide → 委譲/shell → CLI reference。各文書の冒頭に入口へのリンクを置き、実行例はBashと明示する。PowerShellでは必要な引数形式の違いを併記し、未実機検証を隠さない。モデルIDは架空の既定値にせず、接続例と実行保証を区別する。

## 書き方と配布の境界

- 実装済みの操作だけを説明し、受付JSONを処理成功と言い換えない。Thread/Run/Turn/message/operation/node/executionのIDを区別する。
- 子エージェント/サブエージェント/subagentを子Thread上のRunに対応付ける。現行systemがこの用語と手順を十分説明していない事実も明示する。
- この作業ではガイド・案内・判断記録を作る。モデル入力やインストーラの仕様変更は行わない。Agent Guideは自動注入されず、モデルへ読ませるには実在するパスを依頼文で指定する。リポジトリ外へbinaryだけ配置してもdocsは同梱されない。
- 秘密値・実会話・ローカルDBは掲載しない。例のIDは返却値へ差し替えることを明示する。origin tokenを表示・コピーする手順は作らない。
- 採用済みの仕様と実装を照合する。ガイドと実装が食い違う場合、ガイドで未実装を隠さず差異を記録する。

## 作成と検証の順序

1. この全体設計を先にGitへ保存する。
2. 入口と機能別文書を作成し、実装・公開型・既存検証へ照合する。
3. 全35操作、全slash操作、主要設定・状態・対象外の対応を監査する。リンク、例の引数、waitのJSON形、所有者の文脈、受付/完了の区別を確認する。
4. README/docs索引/開発状況を更新し、検証の条件・結果・未実施を [検証状況](../validation/acceptance.md) に記録してローカルcommitする。実会話・資格情報・私的ログを含めず、push・リリースは利用者の明示指示に従う。

文書変更のために全OSテストや有料API試験を反復しない。必要なCLI helpとオフラインで実行可能な利用例を確認し、未実施と文章の照合結果を分ける。

## 検索・出典・関係参照の更新

現行35操作をCLI辞書とF8案内へ反映する。機能別説明はhistory-and-searchへ集約し、Agent Guideは検索scopeの明示と原文・関係の追跡手順を示す。検索のThread省略を自己対象と誤認させず、global hitから所属や採用を推定しない。spansはUnicode抽出本文の意味検索chunk範囲、provenanceは生成元、置換witnessは記録時の出典として説明し、L2距離threshold・fork lineage・祖先/子孫一括探索を実装済みと書かない。

## 操作追跡・実行情報・削除影響

今回の現行35操作ではThread delete-previewをCLI辞書、復旧・保存、Agent Guide、F8案内に載せる。原文32操作や検索拡張時点34操作の経緯は書き換えない。contextの未知時刻省略、lifetime_msの待機込み閉区間、生成元と採用先、復旧と失敗を区別する。operation.getは保存された受付と現在のdownstreamを分け、compactの同ID受付結果が不変なことを明記する。削除影響の参照数をGC到達性や物理空き容量へ読み替えず、実削除の再判定を案内する。詳細は[採用設計](observability-and-deletion.md)。


## 2026-10-25 の配布と到達性の更新

初回のガイド整備ではrepo内パスを指定する配布だったが、[docs/RAG設計](docs-and-rag.md) によりこの制限を置き換えた。現在は同版の利用ガイドをbinaryに同梱し、docs list/search/getでcwdやAPI無しに読む。systemに短い入口を置く。全文の自動注入は引き続き行わない。旧節の『同梱されない』は初回整備時点の記録であり現行の制限ではない。
