# Shellboy

A lightweight terminal AI agent environment written in Rust. Models have one tool: shell. Through the `shellboy` CLI, they can search and fork their own history and recursively delegate work to child agents. Shellboy runs as a single executable, stores data in SQLite, and connects to a Responses-compatible proxy.

## Features

- **One executable** for Windows x64, macOS Apple Silicon, and Linux x64. Conversations and indexes live in local SQLite databases; no separate service or database server is required. A release build started in about 41 ms and used about 17 MiB RSS while idle in a limited, empty-database fixture with no model configured. See the [verification record](docs/validation/acceptance.md) for conditions.
- **Shell is the only model tool.** History, messaging, child conversations, waiting, and attachments all use the same CLI. People can use it from another terminal too.
- **Inspectable history.** A Thread is a conversation, a Run owns ongoing work, and a Turn lasts until one normal response. Models can search past messages and shell output by keyword or meaning, retrieve originals, and follow their relationships. Forks, summaries, and compaction preserve provenance instead of rewriting history.
- **Recursive delegation.** Models can create child Threads, assign work, wait, and read the results. Thread lineage and Run ownership are tracked separately.
- **Durable requests and recovery.** Accepted requests are saved before execution. Communication failures and invalid model responses retain the reason and enter recovery wait, where you can retry, give a new instruction, or stop.

Start with the [User Guide](docs/guides/user-guide.md) ([Japanese](docs/guides/user-guide.ja.md)). Models working inside Shellboy should read the English-only [Agent Guide](docs/guides/agent-guide.md). The [guide index](docs/guides/README.md) covers configuration, history search, attachments, recovery, and the CLI.

English is the default documentation language; only the User Guide has a Japanese translation, `user-guide.ja.md`. The current TUI remains Japanese. UI language options are [evaluated here](docs/design/documentation-language.md); no UI language switch is implemented yet.

The [development status](docs/development/status.md) and [verification record](docs/validation/acceptance.md) distinguish implemented behavior from the checks actually performed.

## Download

[Releases](https://github.com/tya5/shellboy/releases) provide Windows x64, macOS Apple Silicon, and Linux x64 archives with `SHA256SUMS`. The current release is [v0.1.6](https://github.com/tya5/shellboy/releases/tag/v0.1.6).

On Windows, download and extract `shellboy-v0.1.6-x86_64-pc-windows-msvc.zip`. For example, in PowerShell:

```powershell
Expand-Archive .\shellboy-v0.1.6-x86_64-pc-windows-msvc.zip -DestinationPath .
.\shellboy-v0.1.6-x86_64-pc-windows-msvc\shellboy.exe --help
```

Set a connection and model as described in [configuration](docs/guides/configuration.md). Known receipt-budget, index-switching, and large raw-search limits are listed in [known limitations](docs/development/known-limitations.md).

v0.1.6 improves adoption, search, and rendering of long histories, as well as delivery to a slow terminal. RAG adds persistent source exclusions, search-readiness guidance, and indexing-result visibility. The documentation language changes in the working source are not part of the already published v0.1.6 binaries.

The conversation database advances to schema 10 and standalone RAG to schema 3. Existing data migrates on the first write-capable open; older binaries with a lower supported schema cannot reopen it afterward. Take a consistent backup before upgrading. To roll back, restore into a separate data directory following the [storage design](docs/design/storage.md). A running process uses the new executable only after restarting.

Built-in PDF/Office extraction and `--extractor` / `--adapter` have been removed. A complete scan marks old PDF/Office file-source entries as unsupported/removed. Convert externally, then register the text with provenance using `rag ingest`. See the [RAG guide](docs/guides/rag.md) for extracted text versus model interpretations.

## Build and run

Install Rustup and your OS's C/C++ build tools. The toolchain is pinned in `rust-toolchain.toml` and dependencies in `Cargo.lock`.

```sh
cargo build --locked
cargo run --locked --bin shellboy -- --help
cargo run --locked --bin shellboy -- --version
```

Copy [config.example.toml](config.example.toml) to your chosen configuration location and set the connection URL and model ID. Credentials come from the configured environment variable. Do not commit keys or real conversations.

```sh
cargo run --locked --bin shellboy -- --config /path/to/config.toml --data-dir /path/to/data
```

Launching without a subcommand requires an interactive terminal. Owner-dependent CLI commands connect to a running Shellboy instance using the same data directory. Read offline guides with `shellboy docs list`. The [terminal fixture](docs/validation/manual-terminal.md) supports checks without a model API.

Run `sh scripts/check.sh` on macOS/Linux or `./scripts/check.ps1` in Windows PowerShell. See [development setup](docs/development/setup.md) for details.

## Repository layout

```text
src/              Ordinary Rust modules and product entry points
migrations/       Versioned SQLite migrations
tests/            Integration tests and synthetic fixtures
examples/         Local verification fixtures
scripts/          Verification and terminal capture tools
docs/guides/      User and model guides; English defaults; Japanese User Guide
docs/design/      Adopted specifications and implementation designs
docs/development/ Setup, status, and decision records
docs/validation/  Verification procedures and observed results
```

The Cargo package and executable are both named `shellboy`. Start development from the [effective specification](docs/design/effective-spec.md); [AGENTS.md](AGENTS.md) defines development boundaries. Former command names, environment variables, and default paths are not supported. Existing data does not need to be moved; Shellboy uses its own default storage location.

## License

[MIT](LICENSE)
