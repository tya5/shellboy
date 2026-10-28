# Issue #19〜#30と0.1.6

2026-10-28、利用者のgoal: 全件の設計・実装・検証、その後push・0.1.6公開・常用ローカルbinary更新。判断は自律的に行い、根拠と未実施は記録する。個別のIssue提案は設計候補として吟味し、コアを小さく保つ。

## 追跡

| Issue | 採用範囲 | 現在地 |
|---|---|---|
| #19 | 削除確定とcleanup警告、閉じた一覧の結果、重複/過去世代/画像通知/確認キー | 完了 · v0.1.6 / 3 OS CI / 完了コメント / close |
| #20 | 永続glob、dot/self DB/unsupported除外、完全scanのremoved、旧RAG移行 | 完了 · v0.1.6 / 3 OS CI / 完了コメント / close |
| #21 | scope状態/不足/次操作、確認済み未準備だけhybrid warning成功 | 完了 · v0.1.6 / 3 OS CI / 完了コメント / close |
| #22 | source別結果/保存状態/失敗根拠/dry-run量、既存shell readによる結果取得 | 完了 · v0.1.6 / 3 OS CI / 完了コメント / close |
| #23 | 抜粋0〜4096、保存JSON長、既存出典/JSON形維持 | 完了 · v0.1.6 / 3 OS CI / 完了コメント / close |
| #24 | 外部変換→ingest、内部Office/PDF/adapter削除、原本の逐次hash | 完了 · v0.1.6 / 3 OS CI / 完了コメント / close |
| #25 | 経路末尾への採用、CAS/順序維持、重複edge再検査を省く | 完了 · v0.1.6 / 3 OS CI / 完了コメント / close |
| #26 | EXPLAINで根拠があるhot query索引、会話DB v10/独立RAG v3 | 完了 · v0.1.6 / 3 OS CI / 完了コメント / close |
| #27 | 有限readonly semantic ranking、取消・版/出典再確認 | 完了 · v0.1.6 / 3 OS CI / 完了コメント / close |
| #28 | shell出力通知とlifecycle待機の分離、既存ShellOutput条件維持 | 完了 · v0.1.6 / 3 OS CI / 完了コメント / close |
| #29 | 描画再利用・byte差分集計・visible優先resize | 完了 · v0.1.6 / 3 OS CI / 完了コメント / close |
| #30 | 遅いUIでも中継を進める有限配送、表示中streamの購読/集約 | 完了 · v0.1.6 / 3 OS CI / 完了コメント / close |

初期5件の全体checkは73群1,274 passed / 0 failed / 19 ignored。これは追加7件の成功ではない。詳細は[検証状況](../validation/acceptance.md)、実装の判断は[判断記録](decisions.md)に残す。

## 順序と完了条件

1. #19〜#23を検証済みcommitに固定する（完了）。
2. #24は#20のunsupported分類へ重ね、古いOffice/PDF版のremovedと出典付きingestを確認する。#25〜#30は同じ合成fixtureで変更前を測り、production変更へ進む（変更前測定完了）。
3. 通知/検索/描画の所有・上限・取消・遅着・終了を設計と回帰試験で揃える。Coreの新しい表/汎用pool/ジョブengineを作らない。
4. 変更後の同一fixture測定、標準check、CLI/TUI/同梱docs、gitleaksを完了し、各Issueへ採用/見送りと検証結果をコメントする。
5. 0.1.6用version/ガイド/移行注意を整え、mainをpush。3 OSのnative CIを確認してからv0.1.6をタグ付けし、Windows ZIPを含む配布物を公開する。
6. 配布archiveの認証なし取得/チェックサム/同梱内容を確認し、macOS ARM64版でversion/help/オフラインguideを確認する。常用binaryをバックアップして原子的に更新し、配置後も照合する。既定データ/設定/稼働processは変更しない。
7. 公開・CI・binary更新の証拠を記録し、全件の完了条件を確認してgoalをcompleteにする。

実機がないOSはnative CIの成功と実機操作を区別する。外部APIを使わない性能fixtureやローカルHTTP mockを、会社proxy/実モデルでの確認として報告しない。不要なデータ変換や既定DBの再索引は行わない。

## 完了（2026-10-29）

上記1〜7をすべて完了。source/tagは025388c/v0.1.6、製品CIは40712696984、配布は40717582688。配布runner更新42c9519はtagを変更せず既存tagをcheckoutする。全配布物の公開取得・検証、常用binaryのバックアップと原子的配置・配置後照合、12 Issueのコメント/closeまで完了した。[詳細と未検証範囲](../validation/acceptance.md)に証拠を記録。性能の測定範囲や採用見送りを変更せず、既定DBの移行や実データの再索引は行っていない。
