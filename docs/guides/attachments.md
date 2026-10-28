# Attaching files and images

[日本語](attachments.ja.md) · [User Guide](user-guide.md) · [TUI controls](tui.md) · [Guide index](README.md)

Ordinary attachments are references to resolved absolute paths, not file copies. Send preparation reads the path's current contents. Saving a conversation does not save or restore earlier versions of external files.

## Adding a file to a draft

Type these commands manually in the TUI and press Enter:

```text
/attach /path/to/report.pdf
/attach "/path/with spaces/notes.md"
/attachments
```

Relative paths resolve against the current Thread's cwd. Inspect draft IDs, kinds, and paths using `/attachments`, then enter and send the ordinary request text. The first send saves the Thread, request, and attachment references together.

Currently `/attach` uses kind `file`. To send an external path as an image, use the CLI's `--kind image` below; for clipboard images, use `/image`. Do not assume the TUI automatically changes input to image based only on the extension.

## Adding to a saved Thread through the CLI

These are Bash/zsh examples. Replace `THREAD_ID` with the actual Thread ID. Humans in external terminals must specify `--thread`; if using a different data directory from the TUI, add the same `--data-dir /path/to/shellboy-data` to each command. Inside a model shell, the current Thread can be omitted.

```sh
shellboy attachment add --thread THREAD_ID --path /path/to/chart.png --kind image
shellboy attach --thread THREAD_ID /path/to/report.pdf --kind file
shellboy attachment list --thread THREAD_ID
```

Successful add and list both return an array of attachment records in `result`. Even adding one path does not return a single object; retrieve its ID from, for example, `result[0].attachment_id`. Each add/list item includes `attachment_id`, `source_node_id`, `delivery_status`, `path`, `kind`, `selected`, `managed`, and, when applicable, `unavailable_reason`. `delivery_status` is `accepted` (request accepted), `held` (pending application), `applied` (adopted into the path), or `cancelled`. Even applied does not mean the model has read the file.

`attach PATH --kind KIND` aliases `attachment add --path PATH --kind KIND`. Kind is required: use `image` or `file`. CLI relative paths resolve against the CLI invocation's cwd; a separate human terminal may have a different cwd from the Thread.

A CLI addition belongs to a dedicated node that records its source. Additions from a model shell are applied after all results of that tool group; they do not interrupt the current request or attach themselves to the latest human message. They remain held during stopping/recovery wait, and apply after explicit retry of the same Run. Pending additions from a stopped old Run are not silently moved to a new Run; attach again if needed. Preparation occurs for the next model request. Acceptance JSON alone does not prove the model could read the contents; inspect sending and response results too. Adding the same path twice creates separate elements and counts twice toward the request's total capacity.

Earlier images are not reattached to a subsequent message that has no attachments. Earlier attachments are resent at their positions in the original messages; history resends differ from newly adding an attachment now. Repeating the same operation ID does not create extra attachments or source nodes.

## Changes, missing files, and explicit reload

After a file changes, later requests use its current contents. Normally attributes such as size and modification time determine whether RAM conversion cache entries can be reused. There is no complete snapshot guarantee against rewrites preserving attributes or changes during reading.

If you suspect a rewrite that preserved attributes, use this command on a saved Thread:

```text
/reload-attachments
```

Alternatively press lowercase `r` in the `/attachments` management screen. Explicit reload bypasses the cache, checks current contents and hashes, and displays results. It does not by itself send a new conversation request or resume recovery wait.

Before/after hash comparisons are per Thread and attachment ID. The baseline updates together with adoption of a successful send. The original position identifies the version read for this request; changes produce a separate Shellboy update notice containing source and comparison hashes. A resend after successful completion does not announce the same change as new again. A failed request does not become the "last successfully sent content." If a file disappears, is unreadable, or exceeds its individual limit, Shellboy does not substitute old cached content. It supplies the model a text notice with path and reason, then continues with the remaining sendable conversation. If the API rejects a format or the whole request, inspect recovery information and take action.

## Excluding an attachment from sending

```text
/attachments
/detach ATTACHMENT_ID
```

CLI equivalents:

```sh
shellboy attachment remove --thread THREAD_ID --attachment ATTACHMENT_ID
shellboy attachment list --thread THREAD_ID
```

Use IDs to distinguish removal from a draft from exclusion from a saved Thread's send selection. Neither deletes the external file. Original conversation nodes and references remain. Inspect `selected`, `managed`, and `unavailable_reason` for actual send selection and unavailability reasons.

Excluded attachments do not automatically return when capacity becomes available. Reattach by path if needed. A fork inherits send selection at that time; it does not unconditionally share later changes in the original Thread. Attachments in a compacted/replaced interval are also excluded from sending and must be reattached if needed.

## Clipboard images

Copy an image to the OS clipboard, then run `/image` or its alias `/paste-image` in the TUI. This differs from ordinary text paste. A background worker retrieves the image and produces PNG, then adds the prepared image to the original draft. If you switch or edit drafts during this operation, a late result does not silently enter a different draft.

Only clipboard images are saved as managed PNGs under Shellboy's data directory. This managed content allows later reuse of the original clipboard image; it does not copy ordinary external attachments. References from drafts, selected Threads, forks, and in-flight sends protect images from reclamation. Sending holds a pin; eviction from cache does not release the content until actual use ends.

Managed images removed from selection or drafts become candidates for age/capacity reclamation. Even after reclamation, history retains references and the reason for unavailability, but content cannot be restored. Shellboy does not unlink external files during this reclamation. Clipboard availability depends on OS and desktop/remote environment.

## Capacity and format limits

| Setting | Default | What it counts |
| --- | --- | --- |
| `limits.attachment_file_bytes` | 20MiB | Original bytes of one element |
| `limits.attachment_request_bytes` | 40MiB | Total original attachment bytes per request; duplicate references count separately |
| `cache.attachment_base64_bytes` | 64MiB ([Configuration](configuration.md)) | RAM cache of bytes and conversions, separate from API input limits |

1MiB is 1,048,576 bytes. Base64 and JSON are larger than the original. Raising Shellboy limits does not increase proxy/model request-size or image-count limits.

When the total exceeds the limit, attachments already sent as history are excluded oldest first. Newly added attachments remain protected until a successful request completes; Shellboy does not guess their importance and discard them. Adding protected content that alone exceeds the limit is an error. If files grow after acceptance and protected content exceeds the limit, the request is held in recovery wait too. Reduce, shrink, or split the target content, then explicitly retry.

Accepting paths with arbitrary extensions does not mean the model understands their content. Image, PDF, and other file support depend on the proxy/model. Shellboy does not automatically add Office conversion, OCR, or audio/video conversion. For rejection of an unsupported format, convert it yourself into a supported input, exclude the attachment, or choose a suitable connection. There is no automatic model or format fallback.

See [Recovery and storage](recovery-and-storage.md) for retention, reclamation, and quotas, and [History and search](history-and-search.md) for original-content references.
