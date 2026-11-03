# Shellboy v0.7.1

## Before Upgrading

- Stop the owner and back up the complete data directory. The first launch
  upgrades conversation storage to schema 25 and builds an index on saved
  tool groups. On a 2.8 GiB database copy with 41,385 groups, index creation
  took 0.322 seconds in one local measurement; actual times vary.

## Fixes

- History inspection no longer occupies the writer lane, and execution-link
  lookups use the new attempt index without changing their results. On one
  copied database, the same 40-item history-list operation took 39.10 seconds
  before and 0.124 seconds after indexing. These single measurements used
  uncontrolled caches and are not a latency guarantee.

## Release validation checklist

- Run `scripts/check-release-upgrade.py` from the independently verified public
  v0.7.0 binary (conversation schema 24) to the exact v0.7.1 candidate (schema
  25). The check verifies migration 25 and the `history_tool_group_attempt`
  index. See [publication validation](publication.md#v071-synthetic-upgrade-check).
