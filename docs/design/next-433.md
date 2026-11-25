# Core #433: Awaiting an answer to an own ask

## Decision

Implement option A from the accepted #433 design. A reply duty remains open when the owing Thread ends its Turn with a later sent, reply-expected Ask still pending. Store `reply.state = awaiting_own_ask` and the exact blocking Ask ID. At the existing Turn-end settlement, decide this atomically with the saved pending Ask state.

When the blocking Ask is answered or reaches another terminal state, restore the duty to `pending`; the normal later-Turn rule then applies. The Thread wakes only through the existing outcome delivery. Expose the waiting duty and blocking ID through `message list --awaiting`, `message get`, and `thread get.pending_replies`. The guide says an ask back keeps the duty open, `message reply` is the final answer, and interim status goes through `message send`.

## Boundaries

No timer, per-Thread setting, nesting limit, new event fact, or change to duties already answered. UI wording for Studio's reply row is owned by Studio.

## Witnesses

1. Ask back then end the Turn: preserve the duty and name the Ask in all required read surfaces.
2. Answer the blocking Ask: re-owe the duty; another unanswered Turn then ignores it as before.
3. Cancel the blocking Ask: re-owe the duty.
4. A pending Ask created before duty application does not block it.
5. Existing answered duties remain closed and unchanged.
