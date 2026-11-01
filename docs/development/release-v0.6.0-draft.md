# Shellboy v0.6.0

A staged English-default interface, proactive context compaction, Anthropic
Messages support, and new request and resource inspection tools.

## Before upgrading

- **Stop the owner and back up the complete data directory.** The conversation
  database advances from schema 17 in v0.5.0 through 18 to 19. Schema 18 extends
  a derived index for separately recorded summary usage (#133); schema 19 adds
  an index for the latest completed context attempt (#141). Standalone RAG
  remains schema 3. Older binaries cannot open the upgraded database; to roll
  back, restore the backup into a separate data directory.
- **Update the owner and CLI together, then restart the owner.** The local
  protocol remains 2; new commands and fields require the matching build.
  Replacing an executable does not update a running owner. Before 1.0.0,
  compatibility with earlier releases is not guaranteed.

## Display language and terminal width

- English is the default display language, independent of OS locale. To select
  Japanese, add the following to the configuration and restart the TUI; use
  `"en"` to select English. Help, conversation and browser surfaces are
  migrating in stages; saved content and model/user text are not translated
  (#35, #139, #145, #146, #147, #148). Core fallback titles, data-directory and IPC startup
  diagnostics, and new compaction summary headings use English without rewriting
  saved data (#152).

  ```toml
  [ui]
  language = "ja"
  ```
- The TUI measures ambiguous character width at startup and reports it in F1.
  If the terminal does not return consistent measurements, the width is unknown
  and layout reserves two columns. This is a measured terminal capability, not
  a locale guess or a width setting (#139).

## Proactive compaction and usage

- When a completed response reports at least 200,000 input tokens, Shellboy
  attempts a bounded compaction at the next safe boundary, before the following
  request. It reacts to provider-reported usage, not a guessed token count; if
  the provider reports no input count, it does not trigger. It does not
  pre-block a large request. Set `auto_at_tokens = 0` under
  `[compaction]` to disable it, or override the threshold per connection (#125).
- Summary requests may cost tokens and affect prompt-cache reuse. `compact list`
  and ungrouped `usage get` expose separately recorded summary-request usage;
  missing provider counts remain unknown, not zero (#132, #133).

## Connections and request inspection

- Native Anthropic Messages is available with
  `api = "anthropic_messages"` on a connection. Provider thinking and unknown
  blocks are retained and replayed in the same dialect; this is not a claim that
  every provider option or gateway is compatible (#120).
- A Thread's effort is sent as `output_config.effort` for Messages when set;
  `minimal` maps to `low`. Thinking is configured independently. Shellboy adds
  Messages cache markers by default; set `prompt_caching = false` to omit them.
  Markers do not guarantee a cache hit, and live provider behavior requires
  separate verification (#126, #127).
- `session_header_names` controls the Thread UUID headers per connection. With
  session headers enabled, the defaults include both `X-Shellboy-Session-Id`
  and `x-litellm-session-id`; override the list or use an empty list to omit
  them. This can help a LiteLLM gateway partition a cache by Thread, but does
  not guarantee provider cache behavior (#143).
- `shellboy thread request --thread ID` reconstructs a next request from saved
  state without starting an owner, calling a model or writing the database.
  `--attempt` inspects a saved attempt, `--compare-attempt` compares prefixes,
  and `--body` includes the body. Request headers and configured credentials
  are omitted, but the body can contain conversation text or secrets present in
  that text: handle the output as sensitive (#137).

## Other improvements

- The context footer shows saved request-size and cache information; the latest
  matching completed attempt is found with a bounded indexed lookup for long
  conversations (#130, #141).
- Bounded list and status observations, retained-source reading and navigation
  fixes improve inspection and scrolling without promising constant-time access
  to all saved history (#107, #108, #115, #118).
- The bundled [resource usage guide](../guides/resource-usage.md) documents
  reproducible Linux PSS and startup measurements, including fake-model
  comparisons. The published sample measures a v0.5.0 build, not v0.6.0.
  Results are sampled observations, not rankings, peak-memory guarantees or
  predictions of real-model cost or quality (#116).

## Known issues

- Some Japanese wording remains even when English is selected as the default.
  The remaining mixed-language audit and cleanup are tracked in #152.
- A rejected or unsafe proactive compaction can suppress further proactive
  attempts for the same Run. Long Runs can therefore continue to grow; inspect
  usage and compaction results. A bounded retry fix is tracked in #153.

## Fixes and development

- Conversation navigation preserves source position during streamed growth,
  handles reverse input and failed reads more reliably, and clamps at the real
  end rather than a partial read window (#118).
- More bounded saved observations and context-attempt indexing reduce repeated
  work in long histories; reported usage remains an observation, not a billing
  or cache-hit guarantee (#115, #130, #141).

## Downloads and validation

Windows x64, macOS Apple Silicon and Linux x64 archives are intended to include
an executable, LICENSE, configuration example and offline documentation. Verify
all archives and installer scripts against `SHA256SUMS` when candidates exist.

**Draft snapshot:** main `b12a8291b1950a5f7db58de4dccbb12101a2a074` before
2026-11-02 08:30 JST. At the cutoff, confirm whether #149, #150, #153 and #156
landed and update the cut; items not in the cutoff are deferred to the next
release. Later commits require the user's direction. CI, candidate
archives/installers/checksums and upgrade verification are separate gates;
this preparation does not create a tag, publish
a release, replace an installed binary or modify user data.

After the reviewer builds a candidate, run the synthetic upgrade check on a Unix
host with the public v0.5.0 binary and the candidate executable. The fixture root
must not already exist, and must not be a user data directory:

```sh
python3 scripts/check-release-upgrade.py \
  --old /path/to/shellboy-0.5.0 --new /path/to/shellboy-0.6.0 \
  --root /path/to/new-disposable-fixture-dir --old-version 0.5.0 \
  --expected-version 0.6.0 --expected-new-schema 19
```

The script uses synthetic data and a loopback provider. Its successful execution
is not yet claimed by this draft; preserve its output as candidate evidence.
