# Shell execution and output

[Guide index](README.md) · [Agent Guide](agent-guide.md) · [CLI reference](cli-reference.md)

The model uses the `shell` tool for external work. The `shellboy shell ...` CLI observes, supplies stdin to, and stops executions that have already started. There is no `shellboy shell start` command. Humans can run arbitrary commands in their own terminals, but those commands alone are not recorded as Shellboy-owned executions.

## Model tool arguments

These are JSON arguments to a tool call, not JSON to pass directly to the CLI. Unknown fields are rejected. Only `command` is required.

```json
{
  "command": "printf '%s\\n' 'hello'",
  "background": false,
  "stdin_open": false,
  "initial_stdin": "",
  "input_encoding": "UTF-8",
  "output_encoding": "UTF-8"
}
```

| Field | Default | Meaning |
|---|---|---|
| `command` | Required | String executed by the configured Bash/PowerShell |
| `background` | `false` | When `true`, return an execution ID after startup so the model can continue |
| `stdin_open` | `false` | Keep stdin open after initial input; otherwise send EOF after that input |
| `initial_stdin` | None | Startup input string, written before later CLI writes |
| `input_encoding` | `UTF-8` | Encoding used to convert initial stdin into bytes |
| `output_encoding` | `UTF-8` | Initial output display encoding; raw output is stored as bytes |

The `command` examples use Bash. The shell type and executable follow startup configuration; there are no custom `shell` or `cwd` tool arguments. The Thread configuration supplies cwd. To change the working directory, check the configuration and the scope affected by changing it. Do not reuse Bash syntax unchanged in a PowerShell command.

## Foreground and background

For foreground work, the model waits for the tool to complete. This does not stop the entire UI/Control: humans can switch Threads or perform other operations. Background work has the same ownership rules; inspect the returned `execution_id` later. Even after the model's Turn returns its final answer, the Run remains live while it owns shells or child Runs. Normal Run completion requires that no owned work remains. Explicitly stopping the Run or exiting the application stops and reclaims owned work.

```json
{"command":"sleep 2; printf '%s\\n' 'ready'","background":true}
```

Observe the actual execution with these Bash commands. Replace uppercase IDs with returned values.

```bash
shellboy shell get --execution EXECUTION_ID
shellboy shell read --execution EXECUTION_ID --limit-bytes 65536
shellboy wait --conditions-json '[{"type":"shell_ended","execution_id":"EXECUTION_ID"}]' --timeout-ms 30000
```

Choose the next observation from the stored state and result:

| Stored state/result | Next observation |
|---|---|
| starting / running / stopping | Get/read the same execution; wait for shell_ended or shell_output if needed |
| wait timeout | The target continues. Inspect the same execution and keep observing; do not restart the command |
| exited / failed / stopped | Read exit_code, end_reason, output_complete, and trailing output. Terminal does not necessarily mean successful |
| cursor_expired / cursor_encoding_mismatch | Check the target and encoding, then resume reading without a cursor. Previously read bytes may return again |
| output_expired | Check retention state. Repeated reads cannot restore reclaimed raw output |

When a human moves foreground work to the background, the shell stays alive and subsequent tool calls that have not executed are cancelled. If explicitly immediate instructions (`message send --now` or the TUI immediate key) release a foreground wait, also inspect the existing execution. Do not treat release of the tool wait as process termination and execute the same command again.

In that case, the tool result includes `status: "running"`, the continuing `execution_id`, and an intervention `reason` of `"human_backgrounded"` or `"additional_input"`. Each subsequent call, if any, receives a `cancelled` result. The UI reports the target and subsequent cancellations from the same stored results; it does not invent cancellations when no subsequent calls exist. The shell may finish after that notification, so get/read/wait on the same ID for its latest state.

Later calls that were not executed save `status: "cancelled"`, `executed: false`,
a machine-readable `reason`, a human-readable `message`, and any known
`prerequisite` call ID, execution ID, observed state, exit code, output completeness,
and CLI wait-release outcome. These facts describe the prerequisite when its
result was returned; they do not assert its current liveness.

- `previous_wait_released_by_new_input` / `previous_wait_released_by_human_background`:
  inspect the prerequisite's existing execution with `shell get`, `shell read`,
  and `wait`. Do not restart the same command just because its wait was released.
- `previous_call_failed`: inspect retained output and the actual state/exit code
  before deciding whether to retry or change the plan.
- `previous_output_incomplete`: inspect the execution and retained output;
  incomplete output alone does not imply a nonzero exit code.
- `previous_wait_released`: inspect `prerequisite.wait_release_outcome`; a CLI
  wait timing out does not imply that the work it observed failed or stopped.
- `tool_group_validation_failed`, `run_stopped_before_call`, or
  `owner_shutdown_before_call`: fix invalid arguments or respect the stop/shutdown;
  the cancelled call itself has not executed.

There is no time-based automatic foreground backgrounding. Use `background: true`
for intentionally long work, retain its execution ID, and stop owned servers or
other persistent commands when finished. A sequence whose later commands depend
on success can be one shell command (`test-command && next-command`) rather than
separate tool calls; releasing the foreground wait still leaves that shell alive.

Unlike backgrounding, interrupting a Run with Ctrl+C requests stopping it and its owned descendants. Acceptance of a stop request does not mean every PID has been reclaimed; query the terminal Run/shell state. An interrupted model is not automatically resumed merely to deliver a notification.

### Minimal check of background output only (Bash)

For this check, fix the target shell's command to the following string. Add no other output, CLI calls, pipes, or redirects. With ordinary Bash configuration, it writes only the ASCII token `SHELLBOY_BG_READ_V1` to stdout without a newline after two seconds, leaves stderr empty, and exits with code 0.

```json
{"command":"sleep 2; printf SHELLBOY_BG_READ_V1","background":true,"stdin_open":false,"output_encoding":"UTF-8"}
```

These are model shell tool arguments. The model must call this tool directly and retain **the `execution_id` returned by this launch**. Do not replace this procedure with a `shellboy message send` request asking another model to launch it.

**Launching and observing may be two separate steps.** First obtain the ID from the tool result above; then run the observation CLI below in another shell tool call. Variables are not shared between shells, so set `BG_EXECUTION_ID` to the actual returned ID string. A human can also observe from another terminal connected to the same owner. For a nondefault data directory, pass `--data-dir PATH` to each CLI command.

```bash
BG_EXECUTION_ID='EXECUTION_ID_RETURNED_AT_LAUNCH'
BG_READ_CHECK_DIR=$(mktemp -d "${TMPDIR:-/tmp}/shellboy-bg-read.XXXXXX")

shellboy wait --conditions-json "[{\"type\":\"shell_ended\",\"execution_id\":\"$BG_EXECUTION_ID\"}]" --timeout-ms 30000 > "$BG_READ_CHECK_DIR/wait.json"
shellboy shell get --execution "$BG_EXECUTION_ID" > "$BG_READ_CHECK_DIR/get.json"
shellboy shell read --execution "$BG_EXECUTION_ID" --encoding UTF-8 --limit-bytes 65536 > "$BG_READ_CHECK_DIR/read.json"

cat "$BG_READ_CHECK_DIR/read.json"
```

Check top-level `ok:true` in each file, then evaluate the following conditions. If wait times out or fails, investigate the state instead of declaring the check complete. Do not supply a cursor on the first read.

| Observation | Expected value |
|---|---|
| `wait.json`: `result.status` | `satisfied`; this alone does not prove successful exit |
| `get.json`: `result.command` | Exactly `sleep 2; printf SHELLBOY_BG_READ_V1`; also compare command/Thread/Run/Turn with the launching tool result |
| `get.json`: `result.state` / `exit_code` / `output_complete` | `exited` / `0` / `true` |
| `read.json`: `result.execution_id` | Matches the ID retained at launch |
| `read.json`: `result.text` | Exactly `SHELLBOY_BG_READ_V1`, without a newline or JSON |
| `read.json`: `result.raw_bytes` | `19`: the **number of raw bytes** retrieved in this page, not their contents or a cumulative total |
| `read.json`: `result.encoding` / `decode_errors` / `output_complete` | `UTF-8` / `false` / `true` |

Evaluate `result.text` and `raw_bytes` in the saved `read.json`, not the whole screen or the model's explanation. The shell running the observation CLI has its own `execution_id`; do not replace `BG_EXECUTION_ID` with that ID. Additional instructions or messages sent to other Threads do not require launching and observing in a single tool call.

When a shell calls `shellboy message send`, that shell's `execution_id` identifies **the process running the sending CLI**. Its stdout legitimately contains acceptance JSON such as `accepted` / `message_id`. Acceptance does not mean the receiving model has completed launching a shell. Message/run/turn IDs in the acceptance result are not shell execution IDs either. For delegated verification, obtain the target ID from the shell tool result actually returned to the receiving model. Calling the tool directly in this minimal check avoids that distinction.

Launching tool results and `shell get` return the actual command and ownership context. Use the same current release for the CLI and owner.

`shell read` decodes the specified execution's stored stdout/stderr raw bytes. It does not automatically mix in CLI response JSON from another execution. However, if the target command itself calls `shellboy message send` or another CLI, that CLI's stdout really does become part of the target shell's raw output. For unexpected characters, first compare `result.execution_id`, the `command` / `cwd` / `thread_id` / `run_id` / `turn_id` from `shell get`, and shell configuration. Distinguish display confusion from output actually present in raw bytes.

## Stdin, initial input, and EOF

Stdin closes by default. For work that needs later writes, launch with `stdin_open:true`.

```json
{"command":"cat","background":true,"stdin_open":true,"initial_stdin":"first line\n","input_encoding":"UTF-8"}
```

```bash
shellboy shell write --execution EXECUTION_ID --text 'second line'
# Supply input containing a newline through UTF-8 stdin
printf '%s\n' 'third line' | shellboy shell write --execution EXECUTION_ID --text-stdin
shellboy shell eof --execution EXECUTION_ID
```

`--text` does not append a newline. Unlike writing an empty string, `shell eof` closes the pipe. It cannot be reopened after EOF. Stdin is a process pipe, not a PTY/interactive terminal; applications requiring a tty may not work the same way.

Write results include `bytes_written`, `status`, `stdin_open`, and an optional `reason`. In addition to `complete`, statuses include `timeout` / `closed` / `stopped` / `failed`; writes may be partial. Do not automatically resend the entire input after a timeout: assuming nothing was sent could duplicate input. Input is checked in advance for representability in the specified encoding; unrepresentable characters are not silently replaced and sent.

## Raw output, encoding, and cursors

Stdout and stderr are stored as separate raw byte streams and decoded independently for display. Pages support partial byte sequences and output without newlines. Changing the encoding does not overwrite raw bytes. Stored output and display excerpts differ; do not mistake a tool result excerpt for the entire output.

```bash
shellboy shell read --execution EXECUTION_ID --encoding CP932 --limit-bytes 65536
# Continue using result.cursor with the same encoding
shellboy shell read --execution EXECUTION_ID --cursor CURSOR --limit-bytes 65536
```

Inspect `text`, `raw_bytes`, `encoding`, `cursor`, `decode_errors`, and `output_complete`. A cursor is opaque and binds information including execution ID, position, retention generation, and encoding. Do not edit it. To reread from the beginning with another encoding, specify that encoding without a cursor. Mixing a different encoding with an old cursor produces `cursor_encoding_mismatch`. Explicit encoding changes invalidate the associated index; explicitly reindex if needed.

CP932/windows-31j/ms932 are treated as Shift_JIS variants. For UTF-16, specify `UTF-16LE` / `UTF-16BE` with known byte order. Do not rely on ambiguous UTF-16 input conversion. For invalid output bytes, inspect `decode_errors` and reread the raw output with an appropriate encoding.

If output may still grow, wait for updates using the returned cursor.

```bash
shellboy wait --conditions-json '[{"type":"shell_output","execution_id":"EXECUTION_ID","cursor":"CURSOR"}]' --timeout-ms 30000
shellboy shell read --execution EXECUTION_ID --cursor CURSOR --limit-bytes 65536
```

Wait also returns when the target becomes terminal. For an empty trailing page, check `output_complete` and shell state too. Raw reclamation due to retention age/capacity produces `output_expired`; old-generation cursors and similar conditions produce explicit errors. Do not reinterpret them as successful empty output. See [Recovery and storage](recovery-and-storage.md).

## Stopping and storage completion

```bash
shellboy shell stop --execution EXECUTION_ID
shellboy shell get --execution EXECUTION_ID
```

Stop's `accepted:true` means the request was accepted. Confirm terminal state before reporting that stopping has completed. Shell `state` is `starting` / `running` / `stopping` / `exited` / `failed` / `stopped`. `exit_code` has a value only when available. `output_complete` and end reason are separate facts. Even a terminal shell cannot guarantee complete output storage if draining or similar work is incomplete.

Reaching the raw output limit stops the target and records the reason and incompleteness. Shellboy does not hide stored bytes and let a process run indefinitely. A model Origin can operate on shells in its own Run or owned descendants; it cannot write/EOF/stop shells in independent Runs. After an application restart, old PIDs are not used to stop processes.

## PowerShell arguments

Windows uses the configured PowerShell. Write tool commands in PowerShell syntax, for example `Write-Output 'ready'`. CLI environment variable references use forms such as `$env:SHELLBOY_RUN_ID`. Bash's `$SHELLBOY_RUN_ID`, `printf`, and here-documents do not have the same meaning.

Windows PowerShell 5.1 and newer pwsh differ in quoting JSON passed to native commands. Even for small JSON examples, check `shellboy --help` and argument passing in the shell in use. With Windows PowerShell 5.1's stop-parsing syntax, for example, insert actual IDs directly without variable expansion:

```powershell
shellboy --% wait --conditions-json [{\"type\":\"run_ended\",\"run_id\":\"ACTUAL_RUN_ID\"}] --timeout-ms 30000
```

Do not expect `$env:...` to expand normally after `--%`. This PowerShell example has not been verified on a physical Windows machine; execution on each PowerShell version remains unverified. Ordinary text sends can use simple arguments such as `shellboy message send --thread 'ACTUAL_THREAD_ID' --text 'Please check this'`.

## Shell elapsed time

`elapsed_ms` measures supervision from immediately before OS process launch until completion is finalized after the direct child exits, descendant reclamation is confirmed, and output drains. It includes waiting while descendants hold pipes and output backpressure, but excludes later DB persistence waits and result reading. `started_at` / `ended_at` are separate stored timestamps. This field is absent before termination and when an execution is recovered after owner restart; no duration is invented.

### Observing long-running shells (Issue #45)

`shell get --execution ID` and shell entries in `status` expose saved
`started_at`, nullable `last_output_at` (latest retained output sequence),
and `elapsed_ms`. While starting/running/stopping, elapsed_ms is calculated
at read time from the saved start and wall clock, not a process-liveness
check or monotonic measurement. For terminal shells it is the saved completion
duration, if known. No output or reclaimed output gives last_output_at: null.
Status timing does not affect activity_at, sorting, cursor or fingerprint.

Ordinary messages wait for the next tool-group/request boundary and do not release the foreground wait. Human Esc interrupts the Turn, backgrounds a running foreground process, and leaves background shells and owned children alive; it does not mean that unread later calls executed. Ctrl+C/Run stop retains full shutdown of owned work. No time-based backgrounding is enabled.

## OS sandbox wrappers

These are optional OS boundaries, not built-in Shellboy security features. A cwd
or separate worktree is not a permission fence. Wrap **only the model shell** by
setting `[shell] executable`; start the owner normally, outside that wrapper.
Use `exec` at the wrapper's final step and pass `"$@"` unchanged so Shellboy's
Bash arguments and process-group stopping still work. Leave the issued
`SHELLBOY_SOCKET` and Origin environment intact: the CLI needs the owner's Unix
socket (Windows uses a named pipe). Never print or copy an Origin token.

For host restrictions, deny direct network access and permit only a separately
managed **allowlist proxy**. Set `HTTP_PROXY`/`HTTPS_PROXY` for cooperative tools,
but treat the OS denial, not those environment variables, as the direct-egress
boundary. The proxy must reject unapproved hosts, ports and tunnels; prevent
arbitrary CONNECT destinations and proxy-side access to internal services.
Qualify HTTPS CONNECT/DNS/rebinding behavior with your proxy independently.

**Limits:** this configuration applies to every conversation, not per Thread.
Owner-mediated operations still have the owner's privileges: for example,
`attachment add --path` can read outside a shell's read boundary. The example
below deliberately does not restrict reads or comprehensively harden OS IPC.
It checks direct writes/network access, not resistance to every malicious-code
escape. Use a least-privilege account/container/VM for a stronger boundary;
keep secrets out of readable files and the inherited environment. Keep the
wrapper, profile and proxy configuration **outside writable work/temp trees**.

### macOS: tested Seatbelt write and network limits

`/usr/bin/sandbox-exec` is deprecated by Apple, but the following mechanics were
tested on macOS 26.3.1 ARM64. Recheck on your macOS version. First prepare a trusted
allowlist HTTP proxy listening on loopback port 3128 outside the sandbox. The
profile opens only that TCP port and the owner's Unix socket; it does not
implement the proxy's host policy. No production proxy deployment is supplied.

Save the following as a trusted, non-workspace-writable `shell.sb`:

```scheme
(version 1)
(allow default)
(deny file-write*)
(allow file-write*
    (subpath (param "WORK"))
    (subpath (param "TEMP"))
    (literal "/dev/null"))
(deny network*)
(allow network-outbound
    (remote unix-socket (path (param "SOCKET"))))
(allow network-outbound (remote tcp "localhost:3128"))
```

Create the work and scratch directories first. Save this executable wrapper
outside both directories, replacing the three absolute paths. They must be
physical paths: for example use `/private/tmp/...`, not the `/tmp` symlink.
The wrapper resolves the socket directory physically for the same reason.

```sh
#!/bin/sh
set -eu
work='/absolute/physical/project'
scratch='/absolute/physical/scratch'
profile='/absolute/trusted/shell.sb'
: "${SHELLBOY_SOCKET:?This wrapper is for Shellboy model shells}"
socket_dir=$(CDPATH= cd -P "$(dirname "$SHELLBOY_SOCKET")" && pwd -P)
socket=$socket_dir/$(basename "$SHELLBOY_SOCKET")
export TMPDIR="$scratch"
exec /usr/bin/sandbox-exec \
    -D WORK="$work" -D TEMP="$scratch" -D SOCKET="$socket" \
    -f "$profile" /bin/bash "$@"
```

Select it in the owner's configuration (do not launch the owner through it):

```toml
[shell]
kind = "bash"
executable = "/absolute/trusted/wrapped-bash"
[shell.env]
HTTP_PROXY = "http://127.0.0.1:3128"
HTTPS_PROXY = "http://127.0.0.1:3128"
NO_PROXY = ""
```

The local synthetic check used a disposable owner/data-dir and a model fixture,
not a paid provider: work/scratch writes succeeded, a write outside both failed,
an allowlist HTTP proxy returned success for `allowed.test` and rejection for
`denied.test`, and direct TCP to a different loopback service failed. The actual
model shell successfully called `message send` and `wait` through the owner
socket. `shell stop` terminated the wrapped shell and its sleep descendant.
The proxy test is HTTP-only; HTTPS CONNECT and real-provider interoperability
remain unverified. This profile allows reads elsewhere and writes to `/dev/null`;
it is not a confidentiality boundary or a hardened general-purpose sandbox.

### Linux: capability probe, complete recipe still unverified

On Ubuntu/Debian, install bubblewrap with `sudo apt-get install bubblewrap`.
A candidate design is a read-only bind of `/`, writable binds for work/scratch,
and `--unshare-net`. Preserve access to the directory containing
`SHELLBOY_SOCKET`; an isolated network namespace also needs a Unix-socket bridge
to the external allowlist proxy. Merely setting `HTTP_PROXY` cannot restore
network access or enforce host policy. Landlock is another filesystem-policy
option, but is not evaluated here.

The complete wrapper, owner IPC, proxy bridge and stopping behavior have not
been qualified together on Linux, so no executable Linux recipe is provided.
The separate capability-probe result is recorded in the validation notes; it
must not be read as a successful full sandbox acceptance check.

### Windows: no lightweight equivalent wrapper supplied

Windows has no comparably simple standard shell-only wrapper for this guide.
AppContainer needs nontrivial setup; Windows Sandbox is a VM, not a replacement
for `[shell] executable`. These Windows options are **unverified here**:

- Run Shellboy as a dedicated low-privilege local user, with a data-dir owned by
  that user, and use NTFS ACLs to limit readable/writable directories. This also
  restricts the owner, so permit its data-dir, terminal and required API access.
- Have an administrator design default-deny outbound policy with an approved
  allowlist-proxy exception, for example on a dedicated VM. Cover child tools,
  not just `shellboy.exe`; a host denylist or proxy environment alone is not a
  fence. Windows Firewall is not a general DNS-host allowlist, and explicit
  block rules take precedence over allow rules: blanket block plus a port allow
  is not a validated recipe.
- Use a container/VM for stronger isolation, or evaluate a Linux wrapper in WSL2
  after qualifying its namespace, IPC and proxy behavior there. No WSL2 or Windows
  Sandbox networking/ACL recipe has been tested as part of this guide.
