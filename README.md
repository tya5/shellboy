# Shellboy

A lightweight terminal AI agent environment written in Rust. Models have one tool: shell. Through the `shellboy` CLI, they can search and fork their own history and recursively delegate work to child agents. Shellboy runs as a single executable, stores data in SQLite, and connects using Responses (default) or an explicitly selected OpenAI-compatible Chat Completions endpoint.

## Features

- **One executable** for Windows x64, macOS Apple Silicon, and Linux x64. Conversations and indexes live in local SQLite databases; no separate service or database server is required.
- **Shell is the only model tool.** History, messaging, child conversations, waiting, and attachments all use the same CLI. People can use it from another terminal too.
- **Inspectable history.** A Thread is a conversation, a Run owns ongoing work, and a Turn lasts until one normal response. Models can search past messages and shell output by keyword or meaning, retrieve originals, and follow their relationships. Forks, summaries, and compaction preserve provenance instead of rewriting history.
- **Recursive delegation.** Models can create child Threads, assign work, wait, and read the results. Thread lineage and Run ownership are tracked separately.
- **Durable requests and recovery.** Accepted requests are saved before execution. Communication failures and invalid model responses retain the reason and enter recovery wait, where you can retry, give a new instruction, or stop.

Start with the [User Guide](docs/guides/user-guide.md) ([Japanese](docs/guides/user-guide.ja.md)). Models working inside Shellboy should read the English-only [Agent Guide](docs/guides/agent-guide.md). The [guide index](docs/guides/README.md) covers configuration, history search, attachments, recovery, and the CLI.

English is the default documentation language; only the User Guide has a Japanese translation, `user-guide.ja.md`. The current TUI remains Japanese. UI language options are [evaluated here](docs/design/documentation-language.md); no UI language switch is implemented yet.

The [development status](docs/development/status.md) and [verification record](docs/validation/acceptance.md) distinguish implemented behavior from the checks actually performed.

## Download

One-line install (available starting with the next release after v0.3.0):

```sh
# macOS Apple Silicon / Linux x64
curl -fsSL https://github.com/tya5/shellboy/releases/latest/download/install.sh | sh
```

```powershell
# Windows x64: Windows PowerShell 5.1 or PowerShell 7
irm https://github.com/tya5/shellboy/releases/latest/download/install.ps1 | iex
```

The installer verifies SHA-256, installs without sudo, and keeps an existing
binary as `.backup-<timestamp>-<suffix>`. The default directory is `~/.local/bin`
on macOS/Linux and `%LOCALAPPDATA%\Programs\shellboy` on Windows. Override it
with `SHELLBOY_INSTALL_DIR`. Set `SHELLBOY_VERSION` (for example, `v0.3.0`) to
select an archive version, or replace `latest/download` in the script URL with
`download/vX.Y.Z` to use that release's script and matching binary. v0.3.0 itself
has archives but no installer assets. The installer does not edit PATH or shell
profiles; it prints instructions when needed. Read [Upgrading](#upgrading) before
replacing an existing installation.

To inspect the script before running it, download and read it first:

```sh
curl -fsSL https://github.com/tya5/shellboy/releases/latest/download/install.sh -o install.sh
cat install.sh
sh install.sh
```

```powershell
Invoke-WebRequest -UseBasicParsing https://github.com/tya5/shellboy/releases/latest/download/install.ps1 -OutFile install.ps1
Get-Content .\install.ps1
.\install.ps1
```

### Manual Download

Download the archive for your OS and CPU from the [latest Release](https://github.com/tya5/shellboy/releases/latest).
Windows x64, macOS Apple Silicon, and Linux x64 archives include the executable
and offline documentation; `SHA256SUMS` lets you verify the download. Extract the
archive before running `shellboy` (`shellboy.exe` on Windows).
Download `SHA256SUMS` beside the archive and check it before extracting:

```sh
# Linux; on macOS use: shasum -a 256 --ignore-missing -c SHA256SUMS
sha256sum --ignore-missing -c SHA256SUMS
```

```powershell
Get-FileHash .\shellboy-vX.Y.Z-x86_64-pc-windows-msvc.zip -Algorithm SHA256
# Compare with the matching filename's hash in SHA256SUMS.
```

Set a connection and model as described in [configuration](docs/guides/configuration.md).
See [known limitations](docs/development/known-limitations.md) for current limits.

### Upgrading

Stop the owner completely and take a consistent backup of the data-dir before
upgrading. A write-capable first startup may automatically upgrade its databases;
older binaries cannot open newer schemas. For rollback, restore the backup into
a separate data directory rather than opening the upgraded store with an old
binary. Read the target Release's notes for configuration changes and detailed
upgrade instructions. Replacing the executable takes effect after owner restart.

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

The Cargo package and executable are both named `shellboy`. Start development from the [effective specification](docs/design/effective-spec.md); [AGENTS.md](AGENTS.md) defines development boundaries.

## License

[MIT](LICENSE)
