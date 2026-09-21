# iOS conversation navigation

The transcript scrolls beneath the composer and floating conversation controls.
The measured composer/footer height reserves enough trailing scroll content to
keep the last message reachable, including multiline drafts and notices.
Controls retain 44-point rectangular tap targets and individual circular material
backgrounds. Their containing row stays transparent; no white strip covers the
transcript.

Previous/Next treat a user message within the top padding band as the current
message. Explicit jumps preserve their semantic selection until actual vertical
scrolling. Local targets remain usable while streaming projection refreshes;
history loads remain serialized, and a jump pins its row against retention.

The tools button toggles the complete disclosure hierarchy: collapse when any
top-level tool/batch is open, expand when all are closed. Its icon and accessible
label indicate the next action. Expanding includes nested tools and JavaScript
output, and manual disclosure changes update the button immediately. The choice
is retained per conversation when switching threads.

## Evidence

All images and video use synthetic fixtures, iPhone 17 Pro simulator, iOS 26.5.

- [Tools expand/collapse video](media/chat-navigation/tools-toggle.mp4): repeated toggles, including nested details.
- [Navigation video](media/chat-navigation/navigation.mp4): five repeated Next/Previous cycles at original speed.
- [First message, content beneath composer](media/chat-navigation/first-message.png)
- [Next message](media/chat-navigation/next-message.png)

The focused UI checks cover collapse and user-message navigation, repeated
navigation, pages without user messages, streaming follow after Latest, and
five-line composer expansion/draft preservation. A separate regression check
covers three complete tools toggle cycles, nested tool/source expansion, manual
disclosure changes, and preservation across conversation switches.

Five Debug simulator Next/Previous cycles averaged 0.766 seconds per **two-tap
cycle** (0.748–0.784 seconds), including XCTest event synthesis, idle waits and
assertions. This is an automation timing, not touch-to-render latency or a
comparison with the previous app. No real-device performance claim is made.

The Release device build succeeded. The opt-in read-only
`testLiveConversationArrowNavigation` was attempted on the paired iPhone, but
Xcode reported that it was locked and could not launch the test. The available
navigation and photo-history simulators showed sign-in screens. Live-account
correctness and speed therefore remain unverified. Unlock a signed-in paired
phone and run that test with `TEST_RUNNER_NANOCODEX_INBOX_LIVE=1` to complete the
real-history check; it does not send messages or change drafts.
