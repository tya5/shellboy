# Shell foreground observation (#437)

Running `shell.list` items and `shell.get` results include a typed Boolean
`foreground`. It is true only for the exact execution the current Turn is
waiting on, as observed from its live Run's foreground slot. Backgrounding a
shell changes the value to false even while the process stays running. Services
and scheduled shells are false. Non-running results omit the field.

This is owner readback, not a launch setting, saved fact, or new event. The
field is identical in global and Thread-scoped listings and in execution reads.
