# Protocol next (#371)

`thread.list` accepts optional `thread_ids`, an array of up to 200 distinct, nonempty Thread IDs. It returns the usual `ThreadSummary` items for those IDs, including archived Threads, in unspecified order and without a next cursor. Unknown and deleted Threads are omitted; an empty array returns no items. Omit the field to keep the ordinary list behavior. The ID lookup does not use `limit` to truncate results and cannot be combined with a cursor, nondefault order, or list filters.
