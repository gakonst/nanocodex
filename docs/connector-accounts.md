# Direct account connectors

`nanocodex2 connectors catalog` lists providers; `list` checks current connections.
`start github` (or another OAuth provider) returns the existing authorization URL.
Open it in the account consent browser and check `list` after completion. Provider
consent may need browser account authentication. Native API callers can forward
callbacks with an owner key; state, PKCE and account binding are still verified.
The CLI does not export provider tokens or invent a callback listener.

Disconnect one selected account with `disconnect PROVIDER CONNECTION_ID`.
Connect Cloudflare using `cloudflare VAULT_ID [--account-id ACCOUNT_ID]`; first
save its API token through private Vault intake. Never pass a token as an argument.

Remote MCP commands are `mcp-list`, `mcp-create TARGET`, `mcp-start CONNECTION_ID`
and `mcp-disconnect CONNECTION_ID`. Start returns the existing consent URL.
Use `--return-to /profile` on either start command to choose a relative return path.

The TUI `/connectors` command accepts these subcommands and runs locally without
a model turn. Arguments are whitespace separated.

`whatsapp-start +15555550123 --operation-id UUID` starts one attempt and returns
safe status metadata only. Reuse the same UUID after uncertainty. In the TUI,
`/connectors whatsapp-pair UUID` opens the private pairing panel for that same
attempt. Enter its code in WhatsApp's **Linked devices → Link with phone number**,
then close the panel and use `/connectors list` to verify connection. Pairing
codes are never included in CLI JSON or conversation history. The mobile app's
existing private pairing panel uses the same account API.

`/connectors chatgpt-start` opens a private device-login panel. Authorize the
displayed device at OpenAI, then press F5 to poll the existing attempt. F5 never
starts a replacement login. `/connectors chatgpt-status` resumes checking it.
Codes clear on focus loss, cancellation, expiry and account/session changes.
Native credential forms are also available through `/vault add openai` and
`/vault ssh-add REFERENCE`.

Writes are dispatched once, and errors do not reflect server error bodies.
After a transport error or failed write, inspect account status before retrying.
