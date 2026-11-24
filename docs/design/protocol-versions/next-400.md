# Model recovery next actions (#400)

Saved Turn recovery errors distinguish `model_auth`, `model_request_rejected`,
and `model_request_failed`; provider text remains evidence, not a next action.
Recovery and `connection.test` share cause-specific transport hints. Turn work
items in status and retained status observation add optional, bounded `message`
and `hint` fields, and `reason` also recognizes saved reason-only records.
