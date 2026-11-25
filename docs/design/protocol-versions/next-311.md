# Protocol next (#311)

## Scoped live event facts

`events.wait` with `include_text` can also report `receive_age_ms` for a live
attempt, including provider keepalives, without exposing provider payloads or
reasoning text. The age is measured by the owner in milliseconds at readback.

`recovery_transitions` contains committed recovery notifications since the
cursor, identified by Thread and event sequence. Baselines and gaps do not ring
historical bells; a gap requests fresh saved state instead. A page returns at
most 64 recovery transitions and may have more even when changes to the same
Thread coalesced.
