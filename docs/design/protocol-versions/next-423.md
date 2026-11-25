# Protocol next (#423)

## Exact message delivery observation

`message.get` can return a typed `held_by` for a currently held message. Only
`foreground_shell` carries an exact blocking `execution_id`. The live fact is
not saved in the receipt. Existing delivery status and hints remain unchanged.

`wait` accepts `message_delivered` with an exact `message_id`. Check `matched`
for that exact predicate; another condition can satisfy a mixed wait. A bounded
observation does not alter delivery, release a shell, or observe a reply.
