# Protocol next (#445)

## Artifact content reads

New read-only operation `artifact.read` (`ArtifactReadArgs {thread_id, artifact_id,
version, path?}`) returns one regular file of the exact saved version as
`ArtifactContent {artifact_id, version, path?, raw_bytes, base64}`. The version
URI's scheme decides what may be read: a `shellboy-app:` absolute folder serves
files by relative `path` and refuses paths or links that leave it; a `file:` URI
or schemeless absolute path is that one file and refuses a `path`; any other URI
is refused. The read is bounded by `limits.attachment_file_bytes` and by one
reply frame. It is receipt-free and has a CLI binding, `shellboy artifact read`.
Errors and bounds: [Thread artifacts](../thread-artifacts.md#reading-content-445).

`artifact.add` and `artifact.update` keep storing URIs unchanged; the CLI now
also joins the relative folder of `shellboy-app:./dir` to its working directory.
