# Documentation

Product users start with the [User Guide](guides/user-guide.md) ([Japanese](guides/user-guide.ja.md)). Models running inside Shellboy use the English-only [Agent Guide](guides/agent-guide.md). Developers start with the [effective specification](design/effective-spec.md).

English is the default for documentation. Only the User Guide has a Japanese translation, `user-guide.ja.md`; unsuffixed guide files are English. Existing Japanese design and historical records remain available; this language change does not claim to translate the entire development archive. New documents are written in English. See the [language policy and UI evaluation](design/documentation-language.md).

| Directory | Contents | Maintenance rule |
| --- | --- | --- |
| `assets/` | Public documentation images | Record source provenance, user selection and optimization evidence in validation. |
| `guides/` | Product operation guides | Match the actual CLI, configuration, and display. `src/docs.rs` embeds the English guide bodies, so they are required at build time. Keep the Japanese User Guide aligned when changing the English User Guide; other guides have no translation obligation. |
| `design/` | Adopted specifications and implementation designs | Check public types, configuration, migrations, and implementation; record contract changes first. |
| `development/` | Setup, status, and decision records | Exclude personal configuration, credentials, and private logs. |
| `validation/` | Verification procedures and observations | Separate source, environment, results, and untested cases. A procedure alone is not a passing result. |

Use the [design index](design/README.md), [setup](development/setup.md), [status](development/status.md), [decision log](development/decisions.md), [publication layout](development/publication.md), and [verification record](validation/acceptance.md) as needed.

The public repository's adopted designs are the development baseline. Private handoff material and old review ledgers are not prerequisites for building, developing, or using Shellboy. Moving code does not change a contract; previous results do not fill gaps in current validation.
