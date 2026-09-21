# iOS conversation navigation

The transcript scrolls beneath the composer and transparent conversation controls.
The measured composer/footer height reserves enough trailing scroll content to
keep the last message reachable, including multiline drafts and notices.
Controls retain 44-point rectangular tap targets without material backgrounds.

Previous/Next treat a user message within the top padding band as the current
message. Explicit jumps preserve their semantic selection until actual vertical
scrolling. Local targets remain usable while streaming projection refreshes;
history loads remain serialized, and a jump pins its row against retention.

## Evidence

All images and video use synthetic fixtures, iPhone 17 Pro simulator, iOS 26.5.

- [Navigation video](media/chat-navigation/navigation.mp4): five repeated Next/Previous cycles at original speed.
- [First message, content beneath composer](media/chat-navigation/first-message.png)
- [Next message](media/chat-navigation/next-message.png)
- [Expanded composer](media/chat-navigation/expanded-composer.png)

The focused UI checks cover collapse and user-message navigation, repeated
navigation, pages without user messages, streaming follow after Latest, and
five-line composer expansion/draft preservation.

Five Debug simulator Next/Previous cycles averaged 0.794 seconds per **two-tap
cycle** (0.788–0.801 seconds), including XCTest event synthesis, idle waits and
assertions. This is an automation timing, not touch-to-render latency or a
comparison with the previous app. No real-device performance claim is made.

The Release device build succeeded. The opt-in read-only
`testLiveConversationArrowNavigation` was attempted on the paired iPhone, but
Xcode reported that it was locked and could not launch the test. The available
navigation and photo-history simulators showed sign-in screens. Live-account
correctness and speed therefore remain unverified. Unlock a signed-in paired
phone and run that test with `TEST_RUNNER_NANOCODEX_INBOX_LIVE=1` to complete the
real-history check; it does not send messages or change drafts.
