# Protocol next (#450)

## Draft save and expired scroll anchors

`draft.save` still commits draft and view state together when the requested
anchor is valid. If a non-following anchor is no longer on the selected path,
the owner commits the draft and leaves the last saved view state untouched in
the same receipt transaction. The result is `accepted: true` with
`anchor_refusal: "cursor_expired"`. The optional field is omitted on a normal
save. A refused draft (for example, an already submitted revision) still
fails without changing either draft or view state. Replaying the same
operation returns the original outcome.
