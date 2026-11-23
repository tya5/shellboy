# Protocol next (#348)

The `service.add`, `service.list`, `service.start`, `service.stop`, and `service.remove` operations manage Thread-owned shell services independently of Runs. Unexpected exit disables the service and sends one ordinary notice to its Thread. Thread-owned output rotates, reporting `output_expired` for an evicted cursor and retained offsets on a fresh read. See [Thread-owned shell definitions](../thread-owned-shells.md).
