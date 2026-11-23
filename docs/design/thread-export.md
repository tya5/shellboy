# Thread export

`thread export` writes UTF-8 JSON Lines, with a single LF after each record:

- `header`: `format` is `shellboy-thread-export`, `version` is `1`, and `thread`, `exported_at`, `core_protocol` and `change_position` describe the Thread, export time, wire protocol and first page.
- `node`: `role` is `current` or `replaced_source`; `node` contains the ID, saved order, selected-path ordinal if any, provenance and verbatim stored `payload_json` text. `owner_thread_id` identifies the saved origin when known, not necessarily the exporting Thread.
- `tombstone`: the same node, role and graph metadata, without `payload_json`, plus a `reason` for the missing body. A retained tombstone is still a node and can be on the selected path.
- Both records carry `relations`: `previous` and `next` edges; `replacement_groups` with group ID, saved state and members in stored ordinal order; and `replaced_by` with group ID, source ordinal and output reference. Node references are `{id, scope}`: `internal` means a member when the page was read (including tombstones), `external` means a retained node outside that membership, and `missing` means there is no saved node row. The export does not recreate entirely removed records or chase external references.
- `trailer`: `lines` counts all preceding lines, and `sha256` is the lowercase hexadecimal SHA-256 of their exact UTF-8 bytes including each LF. `change_position` records the last page.

Export is live, not a snapshot. Membership, roles and payloads reflect saved data when each page reads it. A different change position indicates a published Thread change; equal positions do not guarantee no changes, including GC. The CLI writes a temporary file beside the requested output and syncs it before renaming. An error leaves any existing destination intact. Treat the output as private conversation data.
