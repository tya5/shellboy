# Thread attachment reload (#311 slice 4g)

`attachment.reload` (`shellboy attachment reload --thread THREAD_ID`) forces
an owner reread of the Thread's selected attachments, even if file size and
mtime match a cached entry. It reports the reload manifest, selection revision,
notices, exclusions and `sent: false`. Attachment listing and draft reads are
unchanged.

Like `shell.read` with transient raw-output pins, this bounded owner observation
creates no operation receipt. Its worker can temporarily pin the selection and
refresh its cache. Reload does not send to a model, change the saved selection or
successful-send baseline, or publish a new event fact. The TUI's explicit reload
calls the same owner operation.
