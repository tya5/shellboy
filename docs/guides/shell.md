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

When a human moves foreground work to the background, the shell stays alive and subsequent tool calls that have not executed are cancelled. If additional instructions release a foreground wait, also inspect the existing execution. Do not treat release of the tool wait as process termination and execute the same command again.

In that case, the tool result includes `status: "running"`, the continuing `execution_id`, and an intervention `reason` of `"human_backgrounded"` or `"additional_input"`. Each subsequent call, if any, receives a `cancelled` result. The UI reports the target and subsequent cancellations from the same stored results; it does not invent cancellations when no subsequent calls exist. The shell may finish after that notification, so get/read/wait on the same ID for its latest state.

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
