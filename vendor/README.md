# Display patches

These vendored packages are based on the exact crate versions pinned in
`Cargo.toml`; their upstream licenses are included alongside each package.

- `crossterm` 0.29.0: expose a nonblocking read of cursor-position responses
  using its existing event reader, so keyboard and paste events are preserved.
- `unicode-width` 0.2.2: apply the terminal's measured ambiguous-width policy
  across UI and presentation workers, including conservative emoji sequence
  reservation, consistently in ratatui and application layout. A thread-local
  override isolates independent rendering tests.

Keep patches small and reassess them on dependency upgrades; do not use locale
or terminal-name heuristics as a substitute for the probe.
