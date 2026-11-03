# Next release: terminal width configuration removal (draft)

Implementation pending. The `ui.ambiguous_width` option introduced in v0.7.0
will be removed. Delete that key from existing configuration before upgrading:
strict configuration parsing rejects unknown keys. Shellboy will measure terminal
width on each launch without waiting for the reply, using the last successful
measurement for the first frame when available. It will never cache a guess.
