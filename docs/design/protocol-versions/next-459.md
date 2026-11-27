# Protocol next (#459)

## Positioned history outline text facts

`history.read` display outlines no longer insert an ellipsis or a localized
placeholder into projected fields. A field reports `truncated: true` when
its `value` is a retained prefix. An omitted deep structure reports
`omitted: "deep_structure"` with an empty `value`; otherwise `omitted` is absent.
Bounded tool text, event kind, and reclaimed or trim reasons carry
`{ "text": ..., "truncated": ... }`. Clients format these facts using their own
language and glyph policy. Literal ellipses in saved text remain unchanged.
