# Instruction file warnings and live config reconciliation (#377)

`status` Run items and `thread.get` optionally expose `instruction_warnings`, a
list of `{path,reason}` (at most eight). Reasons are `not_found`,
`permission_denied`, `io_failed`, `not_regular_file`, `input_limit`, and
`invalid_utf8`. New Turn preparation skips only the unreadable or invalid
registered common file. Local AGENTS/cwd handling remains unchanged.

`config.set` can return `written: false` and `applied_to_running_owner: true`
when the validated file already holds a value that must be adopted live.
Hash conflicts and the restart-only fields keep their existing rules.
