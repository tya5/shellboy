# Protocol next (#423)

## Remove immediate message delivery

New sends and asks no longer accept `deliver: "now"`. Default delivery still
uses the next safe boundary; `complete` still waits for Run completion. An
ordinary send does not release a foreground shell wait. To intervene, observe
application with `message_delivered`, inspect the message's `held_by`, then
explicitly Background the exact execution named by a `foreground_shell` hold.

## Historical delivery and operation replay

Existing accepted input still reaches the ordinary delivery boundary. Saved
`now` delivery metadata, operation receipts and `additional_input` results
retain their original values. Exact operation replay keeps its original argument
identity and does not deliver or wake again.
