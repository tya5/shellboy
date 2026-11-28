# Owner Docs source registry (#479)

Owners advertise `docs.source.registry` and accept `docs.source.list`,
`docs.source.add`, and `docs.source.remove`. Requests operate on the running
owner's selected config file, including an explicit startup `--config`.
There is no config path argument or fallback. An owner without a selected
config path refuses the request.

List returns `items` and the selected file's `content_hash`. Add takes `name`,
an absolute `path`, and `expected_hash`; remove takes `name` and
`expected_hash`. The registry applies its existing validation, including the
persisted path byte limit. Use the hash from list on the same selected owner. The
existing config writer checks it again before replacement. A stale hash
returns `operation_conflict` with `reason: config_changed` and `written: false`.
Add and remove use ordinary owner mutation receipts. The standalone Docs CLI
remains available for ownerless use.
