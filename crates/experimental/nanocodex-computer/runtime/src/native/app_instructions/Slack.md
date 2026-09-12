## Slack Computer Use

Slack enters typed text into the message composer when no text field is focused. Before pressing 'Return', make sure the intended text field is focused to avoid inadvertently sending a message.

Use `set_value(...)` instead of `type_text(...)` to enter text into the message composer to avoid inadvertently sending a multiline message. `set_value(...)` will include markdown syntax verbatim. To apply markdown formatting after calling `set_value(...)`, use the 'super+shift+f' keyboard shortcut.

For input strings containing '\n':

* `set_value(...)` will not send the message, and instead will insert a newline
* `type_text(...)` **will** send the message

If you need to use `type_text(...)` or `press_key(...)` to enter a new line: Slack allows users to configure whether 'Return' or 'Shift+Return' sends a message. You can tell which is active in the AX text from `get_app_state(...)` by looking for hint text on a button below the composer field with the label: "(key combination) to add a new line". The key combination not listed in the hint will send the message. This hint only shows if the composer contains 3 or more characters, and it only shows in the composer below channels and DMs, not for thread replies or edits.

If the AX text from calling `get_app_state(...)` on Slack is behaving unexpectedly, use screenshots as the source of truth.
