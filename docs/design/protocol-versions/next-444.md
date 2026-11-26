# Protocol next (#444)

## Stop settlement failure on the status Run item

A `status` item with `kind: "run"` can include the same
`stop_settlement_failure: "storage" | "invariant"` value that `run.get` returns
for that Run. It is the same saved Run-owned fact with the same writer; the item
carries it only while that Run is `stopping`, and omits it before a failure and
after successful settlement. A client that lists work can read it from the Run
item without a separate `run.get`. Like other item details, it does not enter
the status fingerprint, cursor or sort keys.
