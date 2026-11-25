# Regenerate the README demo

On macOS, build a trusted `shellboy` binary from the checkout you want to record.
Then run from the repository root:

```sh
TMPDIR=/tmp python3 scripts/dev/record-readme-demo.py --binary /path/to/shellboy
```

The recorder uses the existing PTY cell decoder. It creates a disposable project,
configuration, home and owner database. Using `/tmp` keeps the macOS per-user
temporary directory out of the recording. A scripted, unauthenticated Responses
mock listens on literal loopback only. The parent runs the failing unit test and
runs `shellboy message ask --thread` in the visible shell. The child reads the
sample source and answers the original Ask. The recorder captures the actual
owed-reply count before allowing the child to answer, and captures the routed
reply before letting the parent finish. It then quits, starts a new TUI process,
and reopens the saved diagnosis with `/threads`.

Python 3, Swift/AppKit (macOS command-line tools), and FFmpeg with `palettegen`
and `paletteuse` are recording-only tools. AppKit rasterizes the captured cells
without GUI or screen recording permissions. FFmpeg uses a shared palette and
GIF frame differencing to keep the file small. The artifact has an opaque light
terminal and a dark caption bar for contrast in both GitHub themes. The GIF must
be 20–40 seconds and no larger than 1,000,000 bytes; the script rejects larger
files. Only the caption outside the terminal is added by the recorder.
