# Private input in the terminal

The managed TUI recognizes private browser login, browser takeover, verification
code, bound browser form and Vault intake requests. A live request opens a local
private overlay. `/secure-input` reopens the most recent pending request. Native
sudo continues to use its separately enrolled, command-bound encrypted helper.
Private terminal entry currently requires macOS or Linux process protection;
other platforms reject private entry before allocating input buffers.

The overlay intercepts input before the chat composer, history, transcript,
clipboard and terminal control/export paths. Values are masked and sent directly
to the authenticated private account endpoint. The agent receives a bounded
receipt containing the request ID, outcome and any saved Vault item metadata.
The receipt lets the agent continue in the same browser and verify the result;
filling a form or finishing takeover does not prove sign-in or payment success.

The account, agent and request remain bound throughout the flow. Login review
shows the exact approved origins. The current document and field references are
checked again before filling. A changed page returns control for inspection and
fresh field selection, without applying the obsolete input.

## Terminal controls

The overlay shows a fresh safety token before admitting private input or
returning to the composer. Type it using ordinary keys rather than paste. This
keeps a partially decoded terminal paste from crossing a private-input boundary.
The same quarantine already protects native sudo approval.

Use Tab/Shift-Tab to move between fields, Space for checkboxes, arrow keys for
choices, and Alt-Enter for multiline text. Ctrl-Enter submits the private form;
Esc cancels. The complete review must be visible before approval is enabled.
Focus loss clears private input and cancels the local flow. An uncertain send is
reported as uncertain and is never automatically retried.

Unsupported controls use an explicit browser fallback for the same private
request. This does not implement third-party passkeys or make arbitrary CAPTCHA,
custom payment frames or native application fields available as terminal forms.

## Saving and reuse

For reusable fields, **Save to Vault** starts enabled. F2 opts out for that
submission. F3 opens the Vault picker; F4 classifies an ambiguous field for saving. Verification codes and card security codes are transient. Field
classification uses browser autofill metadata; ambiguous fields can be classified
explicitly in the private overlay. Password-only pages can include an optional
username used only for the Vault entry. A two-step login retains the username in
private memory, bound to the same browser session, target and origin, until its
password is entered or the flow is cancelled or expires.

New entries are written through the encrypted credential broker. Only supported,
complete entries are saved. The result distinguishes a saved item from incomplete
reusable fields and a failed save; it never reports storage merely because the
website input succeeded. Saving uses stable broker operation IDs. A save retry
only retries the Vault write and never repeats browser input. F5 offers this save-only retry after a retryable storage failure. Old clients that
omit the save option continue to use transient input.

Saved logins, API keys, cards, addresses and phone numbers can be injected through
private browser field references or the [Vault HTTP broker](../vault-requests.md).
The caller provides an item ID and field mapping. The host resolves the selected
values after account, item, origin, document and field checks; neither the model
nor ordinary terminal tools receive the resolved material. Reusing an item does
not create another saved copy. Saving an item does not authorize unrelated tasks
or purchases.

## Verification

The synthetic journey runs the built terminal in a PTY, drives the production
private browser runtime against local Chromium, and writes to the real encrypted
Vault broker in workerd. The model events and external website are fixtures.
The terminal journey covers one-time browser login, named Vault takeover, named
verification-code challenges, bound password and typed secure forms, all five
Vault intake/reuse kinds, and mixed text/select/checkbox/multiline controls.
Production account-to-managed HTTP admission is tested separately with synthetic
account enrollment. Evidence includes terminal output, safe receipts, HTTP
assertions and browser state checks; test values must remain absent from model
transport, chat/history and operational logs.

Run from the repository root after building the CLI and JavaScript dependencies:

```sh
cargo build --locked -p nanocodex2-bin --bin nanocodex2 --jobs 3
node --experimental-transform-types js/managed/test/private-input-tui.chrome.mjs
node js/managed/test/private-input-admission-journey.mjs
node --experimental-transform-types js/managed/test/browser-injection.chrome.mjs
```

Set `CHROME_PATH` for an installed Chromium executable and
`NANOCODEX_TEST_BINARY` to use a different built CLI. These journeys use synthetic
credentials only and do not sign in to production accounts, make purchases,
install privileged helpers or establish passkey support.
