# Find a guide

[Guide index](README.md) · [Agent Guide](agent-guide.md) · [CLI reference](cli-reference.md)

`shellboy docs` reads guides bundled into the executable and from folders registered in `config.toml`. Browsing needs no repository, running Shellboy owner, or API connection.

## Browse the hierarchy

```sh
shellboy docs list
shellboy docs list --parent shellboy
shellboy docs list --parent shellboy/work
shellboy docs get --id shellboy/agent-guide
shellboy docs get --id shellboy/delegation
```

The list's `id` and `parent_id` are logical IDs independent of file placement. `shellboy/start` contains introductory guides, `shellboy/work` contains task guides, and `shellboy/reference` contains references. Listing returns direct children; search's `--parent` includes descendants.

## Register a folder

```sh
shellboy docs source add --name team --path ./handbook
shellboy docs source list
shellboy docs source get --name team
shellboy docs source update --name team --path ./new-handbook
shellboy docs source remove --name team
```

Registrations are `[[docs.sources]]` entries with a name and an absolute path. A folder is a menu category, and every `.md` file is a guide. The first heading supplies the title (the filename is the fallback); the first paragraph supplies the summary. There is no frontmatter or special filename. `shellboy` is reserved for the bundled guides. Adding the same name and path is a no-op; use `update` to move it. An unavailable folder remains visible as an unavailable branch. Remove only unregisters the folder; it does not delete files or change a RAG index.

## Search by keyword

```sh
shellboy docs search --query 'child agent'
shellboy docs search --query 'background' --parent shellboy/work
shellboy docs search --source shellboy --source team --query 'recovery'
```

The bundled catalogue is English. Search uses case-insensitive literal substring matching, not query translation. Japanese literals present in quoted UI captions can also match without word spacing. Repeat `--source` to select multiple branches with list or search; it intersects `--parent`. Search scans bundled guides first, then registered folders in config order and files in path order. Scores describe individual hits, not a globally ranked list. A bounded page may contain no hits and still have a cursor. A search excerpt is not the full document. Use its `id` with `docs get` to read the source. Explicitly select `rag search` for semantic search or searches spanning guides and other documents.

## Semantic indexing

```sh
shellboy docs index --source shellboy --dry-run
shellboy docs index --source shellboy
shellboy rag search --scope source:shellboy --mode semantic --query 'recover work'
```

Use `--all` instead of `--source` to select all sources. Dry-run previews bounded Markdown file and byte counts from docs, not expected RAG changes, and writes nothing. Live indexing explicitly hands the whole registered folder to RAG, which applies its own format and exclude rules. Bundled guides are exported into the stable `DATA_DIR/docs-export/shellboy/` root. Semantic indexing may incur embedding charges. Browse and search never start indexing.

## Versions and pagination

list/search return `items` and `next_cursor`; get returns Markdown `text`, `sections`, `links`, and `next_cursor`. To continue, pass `--cursor` with the same ID and section, or the same search conditions. You may change the page size for the next request.

`version` combines the executable version and bundled catalogue hash. Bundled documents include a SHA-256 `content_hash`. get's `start` / `end` are UTF-8 byte positions relative to the whole document; pages never split a character. Use a `sections` ID with `--section`. Reusing a cursor after an update or with different conditions is an error; start a new request.

`links` distinguish `bundled` guide links, `external` web links, and `repository` links available only in the development repository. The latter two are not bundled text. A link to development material does not imply that its file exists in your working environment.

These commands do not operate on an owner or persist operation receipts. They cannot be combined with `--operation-id`. Conversation commands still require an owner running against the same data-dir.
