# Protocol next (#345)

`thread.import {document}` accepts one verified version-1 Thread export as JSONL text. The request, including JSON escaping and the IPC envelope, must fit the existing frame bound. The owner revalidates the document and returns `{thread_id}` after one receipt-backed writer transaction. No Run or Turn is restored.
