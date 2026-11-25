# Reply result schema (#422)

`message.reply` is registered as `MessageReplied`: required `message_id` and
`reply_to`, plus `delivery: MessageAccepted` when the answer is routed to a live
asking Thread. This records acceptance for routing, not completed delivery:
`delivery.delivery_status` may initially be `held`. Use `message get` with the
answer's `message_id` for current status. Standalone answers omit `delivery`.
The owner payload, saved receipts, and Send and Ask result contracts are unchanged.
