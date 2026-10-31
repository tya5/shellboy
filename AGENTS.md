# Shellboy development guide

This file contains instructions for developing Shellboy itself. Product users should start with the [User Guide](docs/guides/user-guide.md); models running inside Shellboy should read the [Agent Guide](docs/guides/agent-guide.md). Linked files are not expanded automatically.

## Working baseline

- Read the [effective specification](docs/design/effective-spec.md) first, then the adopted designs for the area you are changing. Check public types, configuration, migrations, fixtures, and implementation against the design. Resolve and reflect discovered discrepancies in the adopted design before changing code or fixtures, including existing mismatches beyond intentional contract changes.
- Keep the [development status](docs/development/status.md) current. Record verified and untested behavior separately in [acceptance](docs/validation/acceptance.md). An existing document or an earlier successful test does not prove the current change works.
- Make routine decisions autonomously. Record uncertain decisions, their reasoning, impact, and conditions for revisiting them in the [decision log](docs/development/decisions.md).
- Follow the [documentation layout and maintenance rules](docs/README.md). Adopted designs must be usable from this repository alone.
- English is the default for new documentation and development instructions. Only the User Guide has a Japanese translation, `user-guide.ja.md`; other guides, including the Agent Guide, are English only. Follow the [documentation language policy and UI evaluation](docs/design/documentation-language.md). Existing Japanese UI labels are unchanged until a separate implementation task.

## Implementation boundaries

- Use Rust, Ratatui, Crossterm, Tokio, bundled SQLite, and sqlite-vec. Record the reason and validation for changes to the pinned toolchain or dependencies.
- Keep I/O, database work, joins, and waits for queue capacity off the UI's main OS thread. Use the shared async runtime, dedicated database worker, and bounded blocking file workers.
- Expose only the shell tool to models. Use the `shellboy` CLI for history, messaging, children, waiting, and attachments.
- Preserve the ordering of state adoption, persistence, cancellation, and late results in the [Control design](docs/design/control.md) and relevant subsystem designs. For every new table, event, worker, queue, or dependency, document why it is necessary, its bounds, owner, and shutdown responsibility.
- Use ordinary Rust modules. Do not add a generic Repository or independent job engine. Do not hide incomplete behavior behind successful stubs.
- Keep credentials, local databases, real conversations, and private logs out of Git. Do not add compatibility paths for the former product's command names, environment variables, or default storage locations.
- Before 1.0.0, do not add compatibility shims for earlier releases in Core (old data formats, deprecated aliases, legacy keys); Core reads only the current format. Schema upgrades belong only in the isolated startup migration modules (`storage::migrate`, `rag::store::migrate`) so historical conversions can be removed later. Breaking changes are acceptable. Keep the general schema migration runner, `user_version`/ledger validation, and protocol version checks. Compatibility is guaranteed from 1.0.0.

## Validation and handoff

- Run checks appropriate to the change. The standard checks are `sh scripts/check.sh` or `./scripts/check.ps1`; see [development setup](docs/development/setup.md).
- Prefer temporary databases, synthetic fixtures, and local mocks. Do not mix paid API calls or modifications to user data into routine verification.
- Distinguish the environment, command, observed result, and untested cases. A mock does not establish success on an unavailable OS or terminal.
- Make local commits at meaningful boundaries. Push when the user or their delegated reviewer (Claude) approves it, including in the task request. Releases (tags and published GitHub Releases), installed-binary changes and user-data changes still require the user's explicit approval. For existing review work, follow the [review ledger process](docs/development/maintenance.md). Update the development status and leave a clear continuation point.
