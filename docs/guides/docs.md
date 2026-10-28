# Find a guide

[Guide index](README.md) · [Agent Guide](agent-guide.md) · [CLI reference](cli-reference.md)

`shellboy docs` reads operation guides bundled into the executable. It needs no repository, running Shellboy owner, configuration, or API connection. `skill` is reserved for a possible separate feature; it is not the name of this guide reader.

## Browse the hierarchy

```sh
shellboy docs list
shellboy docs list --parent shellboy
shellboy docs list --parent shellboy/work
shellboy docs get --id shellboy/agent-guide
shellboy docs get --id shellboy/delegation
```

The list's `id` and `parent_id` are logical IDs independent of file placement. `shellboy/start` contains introductory guides, `shellboy/work` contains task guides, and `shellboy/reference` contains references. Listing returns direct children; search's `--parent` includes descendants.

## Search by keyword

```sh
shellboy docs search --query 'child agent'
shellboy docs search --query 'background' --parent shellboy/work
```

The bundled catalogue is English. Search uses case-insensitive literal substring matching, not query translation. Japanese literals present in quoted UI captions can also match without word spacing. A search excerpt is not the full document. Use its `id` with `docs get` to read the source. Explicitly select `rag search` for semantic search or searches spanning guides and other documents.

## Versions and pagination

list/search return `items` and `next_cursor`; get returns Markdown `text`, `sections`, `links`, and `next_cursor`. To continue, pass `--cursor` with the same ID and section, or the same search conditions. You may change the page size for the next request.

`version` combines the executable version and bundled catalogue hash. `content_hash` is the SHA-256 of the entire document. get's `start` / `end` are UTF-8 byte positions relative to the whole document; pages never split a character. Use a `sections` ID with `--section`. Reusing a cursor after an update or with different conditions is an error; start a new request.

`links` distinguish `bundled` guide links, `external` web links, and `repository` links available only in the development repository. The latter two are not bundled text. A link to development material does not imply that its file exists in your working environment.

These commands do not operate on an owner or persist operation receipts. They cannot be combined with `--operation-id`. Conversation commands still require an owner running against the same data-dir.
