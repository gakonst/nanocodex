# Stripe Link spend approvals

Connect **Stripe Link** from Account → Connectors, the native inbox, or
`nanocodex connect link`. Open the Link verification page and approve the
connection. Nanocodex polls that exact connection attempt until it succeeds,
is declined, or expires. Multiple Link accounts have separate connection IDs.

Agents discover `link_request` through tool search. The approval journey is:

1. `POST /spend_requests` with `merchant_name`, `merchant_url`, `context`,
   `amount` in minor currency units, and `currency`. Explain the purchase in
   `context` (at least 100 characters). Use `test: true` for test requests.
2. `POST /spend_requests/lsrq_…/request_approval`, then show the returned
   `approval_link` to the user.
3. `GET /spend_requests/lsrq_…` to check status, or
   `POST /spend_requests/lsrq_…/cancel` to cancel.

Approval takes place in Link. This connector does not approve purchases,
retrieve card/payment tokens, or execute checkout. Delegated approval endpoints
and credential expansions are blocked in the credential broker. Writes are not
automatically retried; reconcile an ambiguous result by listing spend requests.
Select `connection_id` explicitly when more than one account is connected.

Apps use the same grant-bound interface:

```js
await client.connectors.link.request({
  connectionId,
  method: "POST",
  path: `/spend_requests/${requestId}/request_approval`,
});
```

The implementation follows Stripe's public
[Link CLI device-auth and spend-request protocol](https://github.com/stripe/link-cli),
including its public client ID and the `userinfo:read payment_methods.agentic`
scopes. Device codes and refresh tokens stay in encrypted broker storage.
There is no redirect callback or client secret for this device flow.

Local validation uses mocked provider responses and the real Worker broker,
including refresh, revocation, account selection, poll backoff, identity
recovery, and approval-only policy. A real Link login and test approval still
need verification against the deployed services; local tests make no purchases.
