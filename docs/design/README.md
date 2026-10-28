# 採用設計

開発は [適用仕様](effective-spec.md) から読みます。通常のCargo packageとして `src/` にmodule、`tests/` に結合試験、`migrations/` に版付きSQLを置き、必要なmoduleだけ子ディレクトリへ分けます。

- 基本構造: [実装](implementation.md)、[runtime](runtime.md)、[Control](control.md)、[状態境界](control-boundary-map.md)
- 公開契約: [protocol](protocol.md)、[CLI](cli.md)、[設定](config.md)
- 保存と履歴: [storage](storage.md)、[history](history.md)、[compaction](compaction.md)、[GC](gc.md)、[索引](index.md)、[末尾採用とhot query](history-adoption-performance.md)
- 外部処理: [モデルHTTP](model-http.md)、[shell](shell.md)、[出力](output.md)、[wait](wait.md)、[添付](attachments.md)、[添付出典](attachment-provenance.md)
- 検索と観測: [検索・関係](search-and-relations.md)、[検索readerとwait通知](history-search-readers-and-waits.md)、[作業状態](work-observability.md)、[操作](operation-observability.md)、[削除影響](deletion-impact.md)、[実行context](execution-context.md)
- 画面: [UI](ui.md)、[構造化表示](tui-presentation.md)、[popup](tui-popup-framework.md)、[スクロール](tui-scrolling.md)、[入力補助](composer.md)、[会話一覧](thread-browser.md)、[モデル一覧](models.md)、[activity](tui-activity-indicator.md)、[復旧ベル](tui-recovery-bell.md)、[描画と配送](tui-render-delivery-performance.md)
- ガイド・文書検索: [ガイド構成](guides.md)、[docs/RAG採用仕様](docs-and-rag-spec.md)、[docs/RAG実装](docs-and-rag.md)

各設計では責務・不変条件・上限・所有者・失敗時の扱いを保ちます。設計の存在と動作検証の成功は別です。[現在地](../development/status.md) と [検証状況](../validation/acceptance.md) を確認してください。
