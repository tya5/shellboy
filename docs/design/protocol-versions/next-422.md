# Reply result schema (#422)

`message.reply` is registered as `MessageReplied`: required `message_id` and
`reply_to`, plus `delivery` containing `MessageAccepted` when the answer is
delivered to an asking Thread. Standalone answers omit `delivery`. The owner
payload, saved receipts, and Send and Ask result contracts are unchanged.
