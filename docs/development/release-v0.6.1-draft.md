# Shellboy v0.6.1

Attachment handling and staged display-language fixes.

## Before upgrading

- **Stop the owner and back up the complete data directory.** The conversation
  database remains schema 19 and standalone RAG remains schema 3, unchanged
  from v0.6.0 at the release cutoff. Before 1.0.0, compatibility with earlier
  releases is not guaranteed; keep the backup for rollback.
- **Update the owner and CLI together, then restart the owner.** Replacing an
  executable does not update a running owner. The local protocol remains 2.
- **Retry a conversation stuck after detaching an attachment and compaction.**
  Once upgraded to 0.6.1, use retry for a conversation blocked by
  `source is outside the selected path`; the fix handles the existing stored
  selection without a data migration (#160).

## Attachments

- Excluded attachments no longer block recovery when compaction has replaced
  their source message. Existing affected conversations can be retried (#160).
- Attachment MIME types are determined from file bytes, not filename extensions.
  Supported text, PDF and image data use their recognized types. Unsupported
  binary content is not sent as `application/octet-stream`; an unavailable
  attachment notice is shown instead (#158).
- Missing or unsupported attachments produce visible notices rather than
  silently disappearing. This is based on synthetic tests; it is not a claim
  that every provider accepts every supported file (#158).

## Recovery

- When a provider stops an incomplete response with `content_filter`, recovery
  displays the categories supplied by the provider and possible next steps.
  Missing or unrecognized categories remain unknown; categories do not establish
  why a match occurred. Retry, rephrase or stop remain explicit choices (#159).

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

**Draft snapshot:** the source cutoff is main
`49d68c63896cbd660740793c07eec88927388cf4` (includes #160 and #159). The
#116 Stage 4 benchmark and the later #153 placeholder-argument change are not
in this cutoff and are not release features. CI, candidate
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
