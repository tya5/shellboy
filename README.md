# Shellboy

Shellboy is a terminal AI assistant for working in a folder. Its conversations,
called **Threads**, are saved locally so you can come back later, inspect what the
assistant ran, and pick up where you left off. The assistant uses a shell to work
with files and commands, and the same `shellboy` CLI you can use to inspect
history and delegate work. Core is a single executable with local SQLite storage.

[Shellboy Studio](https://github.com/tya5/shellboy-studio) is an optional browser
UI that connects to a compatible Core.

## Your first Thread in about five minutes

Five minutes is a target if you already have a compatible model endpoint, not a
setup guarantee. You need its API root URL, a model ID, and a credential unless
you are using a keyless local endpoint. Use the [download for your platform](#download)
and open an interactive terminal with the downloaded `shellboy` on your PATH.

1. Run `shellboy config init` and open the file named by `config_path` in the JSON
   result. Set `default_model` to your model ID and the selected connection's
   `base_url` to its API root, not the URL of an individual request. The template
   uses `default_connection = "proxy"` and `api_key_env = "SHELLBOY_API_KEY"`.
   Keep the variable **name** in TOML, not the key. For a compatible keyless
   local endpoint, remove `api_key_env` and skip the credential step. See
   [Configuration](docs/guides/configuration.md) for endpoint choices and output limits.
2. If using a credential, set the named variable in this terminal. These examples
   prompt without putting the key itself in the command. Model-started shells can
   inherit this environment, so use a key and workspace you trust the assistant with.

   Bash:

   ```bash
   read -r -s -p 'API key: ' SHELLBOY_API_KEY
   printf '\n'
   export SHELLBOY_API_KEY
   ```

   PowerShell:

   ```powershell
   $key = Read-Host 'API key' -AsSecureString
   $env:SHELLBOY_API_KEY = [System.Net.NetworkCredential]::new('', $key).Password
   ```
3. Run `shellboy config check`. It reads the configuration and makes a model
   catalog GET, not a conversation request. Fix errors before continuing; a
   successful check is not proof that the endpoint accepts conversation requests.
4. In a sample directory with a `README.md` you are comfortable sending to the
   endpoint, run `shellboy`. Type the prompt below and press Enter:

   > Read this folder's README and summarize the project in three bullets. Do not change files.

   This sends a model request and may incur provider charges. Once accepted, the
   request appears in the saved conversation instead of the draft. That means it
   was saved, not that the reply succeeded. Wait for the assistant's separate reply,
   or inspect the reason shown if processing fails. On a later launch, press
   Ctrl+T or type `/threads`, select the saved Thread, and press Enter to reopen it.

## Download

Use the [latest release](https://github.com/tya5/shellboy/releases/latest) for
macOS Apple Silicon, Linux x64, or native Windows x64. The installers verify SHA-256
and install without sudo. Download and read the script before running it:

Bash (macOS or Linux):

```bash
curl -fsSL https://github.com/tya5/shellboy/releases/latest/download/install.sh -o install.sh
cat install.sh
sh install.sh
export PATH="$HOME/.local/bin:$PATH"
```

Windows PowerShell 5.1 or PowerShell 7:

```powershell
Invoke-WebRequest -UseBasicParsing https://github.com/tya5/shellboy/releases/latest/download/install.ps1 -OutFile install.ps1
Get-Content .\install.ps1
.\install.ps1
$env:Path = "$env:LOCALAPPDATA\Programs\shellboy;$env:Path"
```

These PATH changes apply to the current terminal. Adjust the directory if you
used `SHELLBOY_INSTALL_DIR` or extracted the binary elsewhere. If PowerShell
blocks the script, you can use the manual download instead of changing your
execution policy. The shell launching Shellboy and the shell used for model
commands are separate choices; see [native Windows and WSL setup](docs/guides/configuration.md#starting-on-each-platform).
Inside WSL, use the Linux build.

For a manual install, download the matching archive and `SHA256SUMS` from the
**same release**. Check the hash before extracting. In the examples, replace
`VERSION` with the version in your downloaded filename and compare the result
with that filename's line in `SHA256SUMS`:

```bash
# macOS Apple Silicon
shasum -a 256 shellboy-VERSION-aarch64-apple-darwin.tar.gz
# Linux x64
sha256sum shellboy-VERSION-x86_64-unknown-linux-gnu.tar.gz
```

```powershell
Get-FileHash .\shellboy-VERSION-x86_64-pc-windows-msvc.zip -Algorithm SHA256
```

Extract the verified archive and put its executable directory on PATH. Check
which binary the terminal finds with `shellboy --version`.

## Use it again

Open Shellboy with the same data directory to find your saved Threads. Use
`/threads` or Ctrl+T to return to one, or `/new` to start a fresh draft. The
interface defaults to English; [Configuration](docs/guides/configuration.md)
explains `[ui] language` and how to select Japanese.

- [User Guide](docs/guides/user-guide.md) ([Japanese](docs/guides/user-guide.ja.md)) for everyday tasks.
- [Recovery and Storage](docs/guides/recovery-and-storage.md) if a request fails or you are unsure whether it was accepted.
- [Guide index](docs/guides/README.md) and [CLI Reference](docs/guides/cli-reference.md) for commands and examples. `shellboy docs list` works offline without an owner.
- [Agent Guide](docs/guides/agent-guide.md) for models working inside Shellboy.

## Compatibility and upgrading

Shellboy is pre-1.0. The [release notes](https://github.com/tya5/shellboy/releases)
for the version you intend to run describe its supported behavior, configuration,
protocol and storage-schema changes. See [known limitations](docs/development/known-limitations.md)
for current limits and [verification records](docs/validation/acceptance.md) for
what has actually been tested.

### Upgrading

Before replacing an installation, read the target release notes, stop the owner
completely, and take a consistent backup of the selected data directory. First
write-capable startup can upgrade its databases. Do not open upgraded data with
an older binary; to roll back, restore the pre-upgrade backup into a separate data
directory. Check the executable version and intended data directory before
starting the new owner. Replacing a file alone does not replace a running owner.

## Development

To build from source, follow [development setup](docs/development/setup.md) for
the pinned Rust toolchain, platform prerequisites, and checks. From the repository,
`cargo build --locked` builds the executable. See the
[effective specification](docs/design/effective-spec.md) and [AGENTS.md](AGENTS.md)
before contributing.

## License

[MIT](LICENSE)
