# Protocol next (#431)

## Run stop settlement failure

`RunDetails` can include `stop_settlement_failure: "storage" | "invariant"`.
This means a stop settlement failed and the same Run is still `stopping`.
An accepted stop receipt records intent, not settlement. The field is omitted
before a failure and after successful settlement. It does not report whether
external work completed or whether every process was reaped. Migration
`next_run_stop_settlement_failure` adds the nullable Run-owned field.
