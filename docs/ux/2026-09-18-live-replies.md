# Live replies while reading history

Incoming agent text appears immediately in the transcript. There is no inline “Load newer messages” button. Reading older content preserves its position; the compact down-arrow returns to the latest reply.

[Simulator recording](media/live-replies-ios.mp4) shows the passing live-arrival regression on iOS 26.5. It uses the local HTTP/SSE history fixture, not a production conversation: scroll to earlier history, retain the reader's position through an incoming reply, then tap the arrow to reach its normal message bubble. The clip is trimmed from XCTest's recording; it is not a separate simulator capture.

Validation: native simulator build and three UI tests pass, covering arrival while reading, streaming-tail following, and forward/backward navigation through twenty pages beyond the retention budget. A deterministic Swift harness exercises the actual model methods for live admission during paging, gap deduplication, empty terminal pages, and snapshot/SSE races.
