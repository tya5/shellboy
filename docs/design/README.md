# Adopted designs

Start with the [effective specification](effective-spec.md). Use ordinary Cargo
layout: modules in `src/`, integration tests in `tests/`, versioned SQL in
`migrations/`, and subdirectories only for modules that need them.

- Architecture: [implementation](implementation.md), [runtime](runtime.md), [OS termination](os-signal-shutdown.md), [Control](control.md), [Revisit](revisit.md), [interrupted tool groups](tool-group-recovery.md), [state boundaries](control-boundary-map.md)
- Public contracts: [protocol](protocol.md), [CLI](cli.md), [configuration](config.md)
- Persistence and history: [storage](storage.md), [history](history.md), [compaction](compaction.md), [GC](gc.md), [index](index.md), [history adoption and hot queries](history-adoption-performance.md)
- External execution: [model HTTP](model-http.md), [shell](shell.md), [output](output.md), [wait](wait.md), [attachments](attachments.md), [attachment provenance](attachment-provenance.md)
- Search and observation: [search and relations](search-and-relations.md), [search readers and wait notifications](history-search-readers-and-waits.md), [work observability](work-observability.md), [standard usage measurements](usage-accounting.md), [operations](operation-observability.md), [deletion impact](deletion-impact.md), [execution context](execution-context.md)
- Presentation: [UI](ui.md), [structured output](tui-presentation.md), [popups](tui-popup-framework.md), [scrolling](tui-scrolling.md), [composer](composer.md), [thread browser](thread-browser.md), [models](models.md), [activity](tui-activity-indicator.md), [recovery bell](tui-recovery-bell.md), [rendering and delivery](tui-render-delivery-performance.md)
- Guides and document search: [guide layout](guides.md), [docs/RAG specification](docs-and-rag-spec.md), [docs/RAG implementation](docs-and-rag.md)
- Documentation language and future UI options: [English defaults and Japanese translations](documentation-language.md)

Each design preserves responsibilities, invariants, limits, ownership and failure
handling. A design document does not establish successful verification. Check the
[current status](../development/status.md) and [acceptance](../validation/acceptance.md).

- [Measured CI test performance](ci-test-performance.md): integration layout,
  coverage inventories, native measurements and watchdog boundaries (#46).

- [Per-Thread instruction references](agents-reference.md): explicit selection,
  validation, audit and new-Turn notification (#51).

- [Interrupted message recovery](message-recovery.md) (#53)
- [Tool cancellation reasons](tool-cancel-reasons.md) (#56)
