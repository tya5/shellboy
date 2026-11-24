# Protocol next (#405)

`shell.read` may return `segments`, a list of half-open UTF-8 byte ranges in
its existing `text`, each labeled `stdout` or `stderr`. Capture and read
cursors, raw-byte accounting, and model-visible excerpts are unchanged.
