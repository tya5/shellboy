# Shellboy guides

Operation guides for people using Shellboy and models working inside it. Start with the [User Guide](user-guide.md) as a person, or the English-only [Agent Guide](agent-guide.md) as a model. A [Japanese User Guide](user-guide.ja.md) is available in the repository and release archives. The executable's `docs` catalogue uses the English guides; it does not implement a language selector.

## Find a guide by task

| Task | Guide |
| --- | --- |
| Start, send a message, or switch conversations | [User Guide](user-guide.md) |
| Learn to operate Shellboy as a model | [Agent Guide](agent-guide.md) |
| Browse, search, and retrieve bundled guides | [Finding guides](docs.md) |
| Find exact text, use semantic search, or estimate historical backfill | [Search without embeddings](rag.md#search-one-conversation-without-embedding) · [Semantic search](rag.md#semantic-search-and-multiple-scopes) · [Index and cost previews](history-and-search.md#index-management-and-cost-previews) |
| Configure an API connection, models, storage, or key bindings | [Configuration](configuration.md) |
| Use keys, slash commands, drafts, or paste | [TUI operations](tui.md) |
| Create subagents, assign work, report, or wait | [Delegation and child agents](delegation.md) |
| Run shell commands and manage input, output, or background work | [Shell](shell.md) |
| Keep a watcher running independently of Runs | [Keep a command running](user-guide.md#keep-a-command-running) |
| Run commands at a set time or on a recurring schedule | [Schedule a command](user-guide.md#schedule-a-command) · [Scheduled checks for agents](agent-guide.md#scheduled-checks) |
| Retrieve history, originals, forks, search, or compaction | [History and search](history-and-search.md) |
| Attach files or images | [Attachments](attachments.md) |
| Recover, stop, reclaim capacity, or diagnose errors | [Recovery and storage](recovery-and-storage.md) |
| Look up command syntax and returned JSON | [CLI reference](cli-reference.md) |
| Read memory/startup observations and reproduce the fake-model workload | [Resource usage](resource-usage.md) |

For people: User Guide → TUI operations → task-specific guides. For models: Agent Guide → delegation/shell → CLI reference. Both audiences share the feature guides.

## Common terms

| Term | Meaning |
| --- | --- |
| Thread | A saved conversation with a cwd, model configuration, and selected history path. |
| Turn | An interval ending in one normal response; it can contain multiple model requests and tool executions. |
| Run | An execution lifetime that owns only its Thread's foreground and background shells. It can remain live after a Turn answers while those shells are active. |
| Child agent / subagent | Another Thread carrying out a request sent as a reply-expected message. |
| Service | A long-running command owned by a Thread, independent of its Runs. |
| Schedule | A command scheduled by a Thread, independent of its Runs. |
| cwd | The Thread's shell working directory, separate from its storage data directory. |
| data-dir / owner | The storage location for the database and execution state / the running Shellboy instance managing that location. |
| node / execution | A saved history item / an identified shell execution. |
| operation / message | A CLI request / saved message text; their IDs are different. |

## Reading these guides

CLI examples use Bash unless stated otherwise. Replace `THREAD_ID`, `RUN_ID`, `NODE_ID`, and similar placeholders with values from actual returned JSON. Read fields inside `result`; check `ok: true` separately from the target's state. Conversation and execution-control commands require an owner running with the same data directory. `docs` and `rag` run independently without starting an owner.

Operation names and states follow the current implementation. Model/proxy capabilities, response quality, and pricing depend on the connection, not these operation descriptions. See [acceptance](../validation/acceptance.md) for untested Windows hardware, GUI terminal, and IME behavior. The TUI defaults to English; set `[ui] language` to `"ja"` for Japanese. See [UI language and diagnostics](user-guide.md#ui-language-and-diagnostics).

The English guides are embedded in the executable. Use `shellboy docs list` to browse, `shellboy docs search --query background` to search, and `shellboy docs get --id shellboy/shell` to read. No configuration, API, or repository is needed. A Thread's `AGENTS.md` is read as work instructions at Turn start; links do not automatically inject guide bodies.

## Development documents

- [Documentation layout](../README.md): guides, designs, development, and verification.
- [Development setup](../development/setup.md): source builds and environment preparation.
- [Guide design](../design/guides.md): audiences, document boundaries, and verification scope.
- [Effective specification](../design/effective-spec.md): adopted functionality and designs.

The repository root's [AGENTS.md](../../AGENTS.md) governs development of Shellboy itself. Models doing ordinary research or other work inside Shellboy use the Agent Guide together with their own Thread's instructions.
