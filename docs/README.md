# 文書案内

製品を使う人は [User Guide](guides/user-guide.md)、Shellboy内のモデルは [Agent Guide](guides/agent-guide.md)、開発者は [適用仕様](design/effective-spec.md) から読みます。

| 配置 | 内容 | 更新規約 |
|---|---|---|
| `guides/` | 同梱する操作ガイド | 実際のCLI・設定・表示に合わせる。`src/docs.rs` が本文を組み込むためビルド時も必要 |
| `design/` | 採用仕様・実装設計 | 公開型・設定・migration・実装と照合し、契約変更を先に記録する |
| `development/` | セットアップ・現在地・判断理由 | 個人環境の設定・資格情報・私的ログを含めない |
| `validation/` | 確認手順と実施状況 | 対象source・環境・結果・未実施を分け、手順だけで合格としない |

[設計一覧](design/README.md)、[開発環境](development/setup.md)、[開発状況](development/status.md)、[判断記録](development/decisions.md)、[公開構成](development/publication.md)、[検証状況](validation/acceptance.md) を用途に応じて参照してください。

公開版はここにある採用設計を基準とします。非公開の引継ぎ資料や過去レビュー台帳をビルド・開発・利用の前提にしません。実装の移動だけで契約を変更せず、未実施の検証を過去の結果で埋めません。
