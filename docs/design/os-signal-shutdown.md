# OS termination and terminal-loss shutdown (#38)

The owner TUI registers termination observation before asynchronous startup.
Unix SIGTERM and SIGHUP request the same draft-preserving priority Quit batch
as Ctrl+Q. SIGINT remains the existing interactive Run-stop key behavior. A
terminal read/draw error also requests shutdown and disables further terminal
I/O while still draining backend updates; it is not a second OS signal.

SIGHUP is an idempotent terminal/session-loss notification, including duplicate
kernel and parent-shell deliveries. It never escalates to force, including when
mixed with SIGTERM. A second explicit SIGTERM may force; this intentionally
changes the former any-two-signals rule. The owner installs its HUP subscription
even if its launching shell inherited an ignored HUP disposition (`nohup` or
`trap '' HUP`). Unix terminal health is additionally checked using a zero-timeout
native poll for HUP/error/invalid descriptor, so EOF on a noncontrolling PTY need
not deliver an OS signal. This does not consume terminal input or replace the
library's parser.

The pinned Crossterm 0.29.0 Unix input source uses its `use-dev-tty`
polling backend. Its default Mio backend loops inside TTY readiness on EOF or
EIO without checking the requested poll deadline, preventing the UI from ever
dispatching Quit after a PTY disappears. The selected existing backend breaks
the read loop on zero bytes, propagates read errors, and retains a finite poll
deadline. Despite the feature name, both backends use the same `tty_fd()` input
selection (terminal stdin, otherwise `/dev/tty`) in this release. Event parsing
and the Windows backend are unchanged. On macOS the selected filedescriptor backend uses select(), which cannot
handle descriptors numbered1024 or above; an environment inheriting that many
open descriptors may fail input initialization and shut down. No file-closing
policy is added for inherited descriptors. The unused event-stream feature is
disabled. This feature reuses the already locked
filedescriptor dependency; no custom parser, extra input worker, or event queue
is introduced. The real PTY-master-loss regression must continue to require
successful cleanup, rather than treating the force deadline as success.

One async observer on the existing Tokio runtime owns OS subscriptions, a bounded
atomic notification word, one Notify, and the existing Diagnostics clone when
an owner becomes available. There is no persistent event/table, independent
worker or unbounded signal queue. Coalesced native signals cannot be counted as
separate deliveries. Before configuration is available, termination uses a five-second
Unix / four-second Windows cleanup budget. Once ready, the Unix budget is at least
five seconds and includes configured TERM grace, two drain timeouts (direct reap
and output drain), 250ms of group confirmation, and the configured shutdown grace
for persistence. Defaults yield 9,250ms. Windows retains its four-second OS callback
cap even if configured shell cleanup would need longer; completed cleanup cannot
be guaranteed beyond an OS deadline. A second explicit TERM or expiry selects
forced exit (code 1), with a short 100ms allowance for the UI to restore
its terminal before an independent runtime observer enforces process exit. This
fallback also works when a terminal write blocks. Terminal restoration during
force is best effort: a stuck write can prevent restoration within this bounded
allowance, so raw mode may need `stty sane` on Unix. Extending the allowance cannot
guarantee restoration under blocked terminal I/O. Normal successful cleanup exits
0. OS-imposed shorter limits, SIGKILL and power loss remain startup-recovery cases.

The UI remains the owner of composer state: Quit freezes/saves its existing
snapshots and closes new tool-start admission. Frontend/Control shutdown preserves
interruption facts, stops owned processes and saves tool outcomes through existing
paths. Rendering failure cannot cancel that drain. Startup termination uses the
existing cancellation/finalization path, including an owner that finishes
initialization concurrently with cancellation. The observer's lifetime is one
owner invocation; normal exit cancels observation. Windows unregisters the native console hook;
Tokio Unix handlers may remain installed until process exit even after stream
drop, so this API is for one owner process invocation, not reusable owner sessions.

Before forced process exit, the live ShellSupervisor closes spawn admission and
best-effort KILLs its known Unix process groups. One short mutex-protected set
holds at most one PGID per active supervisor task; spawn/publication is serialized
with force cleanup. Confirmed group disappearance immediately disarms/removes the
entry before output-drain awaits. Unconfirmed cleanup or supervisor abort makes
one final best-effort KILL before removing the entry; numeric PGIDs are not retained
after native ownership ends. Forced cleanup
uses try_lock and never waits behind native creation: an atomic force flag makes
a racing spawn KILL its own just-created group before publishing. The observer
retries its best-effort snapshot before final process exit. An OS spawn still
stalled at that instant cannot be promised cleanup. No PGID is loaded
from saved state, no additional task or queue is added, and process-group identity
is still an OS race rather than a durable capability. Windows kill-on-close Job
handles are closed by process exit. This does not claim durable completion or
successful reap on a force path.

Reuse existing best-effort Shutdown diagnostics with fixed signal scope/stages;
no path, arbitrary error or signal-handler text is queued. Signals observed before
a diagnostic writer exists cannot be promised durable logging. Existing shutdown
stage traces show cleanup progress. The frontend Control end trace includes a
fixed product error_code when draining failed, so reaching the end stage does not
misrepresent successful persistence after the terminal disappears. No arbitrary
DB/OS error message is logged. Forced termination may prevent final writes.

Windows uses the existing fixed windows-sys dependency with its Console feature,
not a new crate. The OS console-handler thread records bounded state and waits
on one completion event for at most four seconds, allowing async cleanup before
returning: returning immediately from CLOSE/LOGOFF/SHUTDOWN would itself permit
OS termination. The one kernel event is retained for process lifetime to avoid
racing an active native callback with handle destruction. It is not a worker or
application queue. Ctrl+C/Break are not intercepted by this close handler.

Console close is supported where the OS delivers it. Logoff/shutdown delivery is
best effort: Windows may terminate interactive applications before these events;
loading user32/gdi32 can suppress them. A hidden window/message-pump subsystem is
not added. Native Windows close/logoff behavior is unverified without a real
console session. Microsoft's [HandlerRoutine contract](https://learn.microsoft.com/en-us/windows/console/handlerroutine)
and [SetConsoleCtrlHandler limits](https://learn.microsoft.com/en-us/windows/console/setconsolectrlhandler)
are the basis for this boundary. No success is inferred from cross-compilation.

Validation uses temporary data, local Responses fixtures and actual Unix binary
PTY processes: TERM, HUP, real bash/zsh parent-shell master loss with a
TERM-ignoring native group, EOF without a controlling-session HUP, configured
longer native grace with mixed TERM/HUP, and repeated-TERM force while DB cleanup
is intentionally held. Verify saved tool results and terminal Run/shell state,
fixed diagnostic stages and no native-shell leftovers. A separate observer
subprocess deliberately stalls its main thread to verify deadline enforcement
without conflating the bound with SQLite busy-timeout errors. The
existing SIGKILL recovery tests remain a separate contract.

Review correction: the earlier report of a normal bash-window-close orphan
(M2) was withdrawn in [the reviewer correction](https://github.com/tya5/shellboy/issues/38#issuecomment-6501975229).
Its process-wait script had treated a leading-dash pgrep pattern as an option
and inspected still-running cleanup. The corrected baseline already completed
bash cleanup. The actual remaining regression is duplicate HUP forcing zsh
closure prematurely (M1). The bash/zsh native tests verify complete shutdown,
but do not claim a newly fixed normal-exit orphan defect. Force-path KILL and
fixed shutdown-error diagnostics are separate bounded hardening/observability.
