# 実装計画と契約照合

初期実装をP1〜P6に分けた計画。機能範囲を縮めず段階ごとにコード・試験・根拠を確認する。現在の採用範囲と未検証の扱いは [適用仕様](effective-spec.md) に従う。初期引継ぎ資料と内部の進捗・受入台帳は非同梱。

## P1の成果物

| 契約 | 実装・成果物 | 検証 |
|---|---|---|
| CLI全操作・状態・error・wait・cursor | `src/protocol.rs`、`protocol.md` | `tests/protocol.rs` |
| TOML全節・型・既定値・OS保存先 | `src/config.rs`、`config.md` | `tests/config.rs` |
| 版付きDDL・索引・vec0・保存worker | `migrations/`、`src/storage.rs`、`storage.md` | `tests/storage.rs` |
| Responsesの保存/replay・完了境界 | `src/model.rs`、`adapters.md`、`tests/fixtures/responses/` | `tests/model.rs` |
| OS/shell起動・停止・文字コード | `adapters.md` | P2/P3で実行fixture・OS試験へ接続 |

## 実装順序

1. P1：公開型、設定、migration、adapter fixtureを照合。未対応箇所を明示し、runtime実装の前に矛盾を解消する。
2. P2：data-dir所有lock、DB、独立Control、ThreadDriver、Responses通信、shell、保存、TUIの縦経路。副作用開始前の保存と明示復旧を試験する。
3. P3：所有木、wait循環、追加入力、背景化、stdin/cursor、子・別チャット・自己操作、終了・再起動。
4. P4：DAG選択経路と共有、原文参照、spill/summary、埋め込み・検索・GC・容量制御。
5. P5：添付、managed画像、クリップボード、下書き/anchor、ページ読込み、入力/表示cache。
6. P6：A01〜A20、V1〜V7、障害注入と性能確認。実行できない実機条件は未実施のまま残し、完了を称さない。

## 採用する内部判断

- CLIの同名操作はドット区切りのwire名（例 `thread.list`）、人間CLIでは空白区切りにする。protocol v1は長さprefix付きJSON。runtime・UI・CLIで操作型を共用する。
- 最終CLI契約のwait既定30秒を採用。主仕様§6の旧「期限省略は無期限」は後の最終契約に置換される。
- Threadの採用経路は順序付きnode参照として保存する。edgeから曖昧に逆算せず、fork/圧縮のsnapshotを再現する。本文は共有する。
- worker間のコマンドを有限キューで受ける。UIはtry_send、async呼出し側は待機可能。停止・取消しは大量本文のキューから独立させる。
- DB保存とネットワーク/OS実行のatomic commitは主張しない。意図を先にcommitし、再起動時に未確認処理を自動再実行しない。

仕様や互換性の不足を発見した場合は、該当する契約文書と判断記録を先に修正して実装を進める。型・fixtureの存在は製品機能の完成証拠ではない。
