# NanocodexChatUI

The optional Nanocodex presentation for `NanocodexChat`. The mobile app and this
package consume the same composer, message and activity view code. It preserves
the app's native editor, expanding composer, send/stop circle, focus ring, blue
user messages, Markdown, activity disclosure, header material and sidebar rows.
The unstyled `NanocodexChat` package remains independent of this presentation.

Requires iOS 18+ for the complete project content. Message/activity views also
support macOS 15+. Add this local package alongside `NanocodexChat`, `NanocodexUI`
and `InboxCore`.

```swift
ProjectConversationView(store: store) { state in
    NanocodexProjectConversationContent(store: state, projectTitle: "DJ Booth") {
        // Host-owned attachment button, using ChatComposerControlLabel.
        attachmentButton
    } voice: {
        // The host's configured NanocodexVoiceControl, or EmptyView().
        voiceControl
    } actions: {
        projectActions
    } drawerFooter: {
        accountActions
    } tasks: {
        projectTasksButton
    } media: { row in
        // Host-resolved authenticated images, video and attachment previews.
        attachmentPreviews(row)
    }
}
```

The content view handles its drawer, composer expansion, draft binding, history,
retry and send/stop actions against the existing store. Use exactly one
`ProjectConversationView` lifecycle owner. Host slots supply authorized upload,
voice, task, project and account actions and authenticated media previews; this package does not invent permissions
or add transport capabilities. The text-only store's existing grant limitations
still apply.

For custom layouts, use `NanocodexMessageContent` (with host media/delivery slots),
`NanocodexActivityView`, and the shared `NanocodexUI` composer/navigation primitives.
Captured-context parsing stays with the host. The mobile app continues to own its
existing advanced media, steering and viewport behavior.

The Debug gallery's **Nanocodex** option uses this content view. Launch with
`--component-gallery --component-native-style` for full-screen native presentation,
and add `--component-dark` for dark-mode evidence. Gallery account/attachment/voice
controls use fixture host handlers; they do not perform authenticated operations.
