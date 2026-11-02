# Shellboy v0.6.1

Attachment handling and staged display-language fixes.

## Before upgrading

- **Stop the owner and back up the complete data directory.** At the current
  draft base, the conversation database remains schema 19 and standalone RAG
  remains schema 3, unchanged from v0.6.0. Recheck this against the final release
  source before publishing. Before 1.0.0, compatibility with earlier releases is
  not guaranteed; keep the backup for rollback.
- **Update the owner and CLI together, then restart the owner.** Replacing an
  executable does not update a running owner. The local protocol remains 2.

## Attachments

- Attachment MIME types are determined from file bytes, not filename extensions.
  Supported text, PDF and image data use their recognized types. Unsupported
  binary content is not sent as `application/octet-stream`; an unavailable
  attachment notice is shown instead (#158).
- Missing or unsupported attachments produce visible notices rather than
  silently disappearing. This is based on synthetic tests; it is not a claim
  that every provider accepts every supported file (#158).

## Display language

- More overlay headings, controls and confirmations follow the configured
  English or Japanese display language. Saved content and model/user text are
  not translated (#149).
- Some generated diagnostic messages use English instead of Japanese in the
  English-default interface (#152). The language transition is not complete.

## Known issues

- Some Japanese and English wording is still mixed in the interface; remaining
  language work is tracked in #150, #152 and #157.
- Model text and reasoning within a single long Turn are not covered by normal
  summarization and can continue to grow the context.

## Downloads and validation

Windows x64, macOS Apple Silicon and Linux x64 archives are intended to include
an executable, LICENSE, configuration example and offline documentation. Verify
all archives and installer scripts against `SHA256SUMS` when candidates exist.

**Draft snapshot:** the release cutoff is not yet selected. This draft currently
covers only changes confirmed on main at its preparation base. CI, candidate
archives/installers/checksums and upgrade verification are separate gates; this
preparation does not create a tag, publish a release, replace an installed binary
or modify user data.

After the reviewer builds a candidate, run the synthetic upgrade check on a Unix
host with the public v0.6.0 binary and the candidate executable. The fixture root
must not already exist, and must not be a user data directory:

```sh
python3 scripts/check-release-upgrade.py \
  --old /path/to/shellboy-0.6.0 --new /path/to/shellboy-0.6.1 \
  --root /path/to/new-disposable-fixture-dir --old-version 0.6.0 \
  --expected-version 0.6.1 --expected-new-schema 19
```

The script uses synthetic data and a loopback provider. Its successful execution
is not yet claimed by this draft; preserve its output as candidate evidence.
