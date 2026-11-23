# Thread export

`thread export` writes UTF-8 JSON Lines, with a single LF after each record:

- `header`: `format` is `shellboy-thread-export`, `version` is `1`, and `thread`, `exported_at`, `core_protocol` and `change_position` describe the Thread, export time, wire protocol and first page.
- `node`: `role` is `current` or `replaced_source`; `node` contains the ID, saved order, selected-path ordinal if any, provenance and verbatim stored `payload_json` text. `relations` gives `previous`, `replaces` and `replaced_by` IDs; `owner_thread_id` identifies the saved origin when known, not necessarily the exporting Thread.
- `tombstone`: `id` and `reason` for a retained node whose body is unavailable. Data removed entirely is not exported.
- `trailer`: `lines` counts all preceding lines, and `sha256` is the lowercase hexadecimal SHA-256 of their exact UTF-8 bytes including each LF. `change_position` records the last page.

Export is live, not a snapshot. Membership, roles and payloads reflect saved data when each page reads it. A different change position indicates a published Thread change; equal positions do not guarantee no changes, including GC. The CLI writes a temporary file beside the requested output and syncs it before renaming. An error leaves any existing destination intact. Treat the output as private conversation data.
