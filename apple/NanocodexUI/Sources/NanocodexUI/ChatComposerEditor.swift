#if os(iOS)
import SwiftUI
import UIKit
import UniformTypeIdentifiers

/// A growing native editor. UIKit owns selection, scrolling and keeping the caret visible.
public struct ChatComposerEditor: UIViewRepresentable {
    @Binding private var text: String
    @Binding private var focused: Bool
    @Binding private var overflowing: Bool
    private let visibleLines: Int
    private let expandsToFill: Bool
    private let onPasteImages: (([NSItemProvider]) -> Void)?

    public init(text: Binding<String>, focused: Binding<Bool>, overflowing: Binding<Bool>, visibleLines: Int = 5, expandsToFill: Bool = false,
                onPasteImages: (([NSItemProvider]) -> Void)? = nil) {
        _text = text
        _focused = focused
        _overflowing = overflowing
        self.visibleLines = max(1, visibleLines)
        self.expandsToFill = expandsToFill
        self.onPasteImages = onPasteImages
    }

    public func makeCoordinator() -> Coordinator { Coordinator(self) }

    public func makeUIView(context: Context) -> UITextView {
        let view = ComposerTextView()
        view.visibleLines = visibleLines
        view.onPasteImages = onPasteImages
        view.onOverflow = { [weak coordinator = context.coordinator] in coordinator?.reportOverflow($0) }
        view.backgroundColor = .clear
        view.font = .preferredFont(forTextStyle: .body)
        view.adjustsFontForContentSizeCategory = true
        view.textColor = .label
        view.textContainerInset = UIEdgeInsets(top: 8, left: 0, bottom: 8, right: 0)
        view.textContainer.lineFragmentPadding = 0
        view.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        view.delegate = context.coordinator
        view.accessibilityLabel = "Ask Nanocodex"
        return view
    }

    public func updateUIView(_ view: UITextView, context: Context) {
        context.coordinator.parent = self
        (view as? ComposerTextView)?.visibleLines = visibleLines
        (view as? ComposerTextView)?.onPasteImages = onPasteImages
        // Reassigning text on every streamed response resets native selection and scrolling.
        if view.text != text { view.text = text }
        if focused != view.isFirstResponder {
            if focused { view.becomeFirstResponder() }
            else { view.resignFirstResponder() }
        }
    }

    public func sizeThatFits(_ proposal: ProposedViewSize, uiView: UITextView, context: Context) -> CGSize? {
        guard !expandsToFill, let width = proposal.width, width > 0 else { return nil }
        let lineHeight = (uiView.font ?? .preferredFont(forTextStyle: .body)).lineHeight
        let insets = uiView.textContainerInset.top + uiView.textContainerInset.bottom
        let maximumHeight = (uiView as? ComposerTextView)?.lineMetrics().height ?? (ceil(lineHeight) * CGFloat(visibleLines) + insets)
        let measured = uiView.sizeThatFits(CGSize(width: width, height: .greatestFiniteMagnitude)).height
        return CGSize(width: width, height: min(maximumHeight, max(ceil(lineHeight + insets), measured)))
    }

    public final class Coordinator: NSObject, UITextViewDelegate {
        fileprivate var parent: ChatComposerEditor
        private var reportedOverflow: Bool?

        fileprivate init(_ parent: ChatComposerEditor) { self.parent = parent }

        fileprivate func reportOverflow(_ value: Bool) {
            guard reportedOverflow != value else { return }
            reportedOverflow = value
            // Layout may not mutate SwiftUI state. Coalesce native layout changes.
            DispatchQueue.main.async { [weak self] in
                guard let self, self.reportedOverflow == value, self.parent.overflowing != value else { return }
                self.parent.overflowing = value
            }
        }

        public func textViewDidChange(_ textView: UITextView) { parent.text = textView.text }
        public func textViewDidBeginEditing(_ textView: UITextView) { if !parent.focused { parent.focused = true } }
        public func textViewDidEndEditing(_ textView: UITextView) { if parent.focused { parent.focused = false } }
    }
}

final class ComposerTextView: UITextView {
    var visibleLines = 5
    var onOverflow: ((Bool) -> Void)?
    var onPasteImages: (([NSItemProvider]) -> Void)?

    override func canPerformAction(_ action: Selector, withSender sender: Any?) -> Bool {
        // Query only availability while building the edit menu. Read image
        // providers only after the user invokes Paste.
        if action == #selector(paste(_:)), onPasteImages != nil, UIPasteboard.general.hasImages { return true }
        return super.canPerformAction(action, withSender: sender)
    }

    override func paste(_ sender: Any?) {
        if onPasteImages != nil, handleImagePaste(UIPasteboard.general.itemProviders) { return }
        super.paste(sender)
    }

    @discardableResult
    func handleImagePaste(_ providers: [NSItemProvider]) -> Bool {
        guard let onPasteImages else { return false }
        let images = providers.filter { $0.hasItemConformingToTypeIdentifier(UTType.image.identifier) }
        guard !images.isEmpty else { return false }
        onPasteImages(images)
        return true
    }

    func lineMetrics() -> (height: CGFloat, overflow: Bool) {
        var lines = 0
        var height: CGFloat = 0
        textLayoutManager?.enumerateTextLayoutFragments(from: nil, options: [.ensuresLayout, .ensuresExtraLineFragment]) { fragment in
            for line in fragment.textLineFragments {
                lines += 1
                guard lines <= self.visibleLines else { return false }
                height = max(height, fragment.layoutFragmentFrame.minY + line.typographicBounds.maxY)
            }
            return lines <= self.visibleLines
        }
        let minimum = (font ?? .preferredFont(forTextStyle: .body)).lineHeight
        return (ceil(max(minimum, height) + textContainerInset.top + textContainerInset.bottom), lines > visibleLines)
    }

    override func layoutSubviews() {
        super.layoutSubviews()
        // Report the actual text container, never SwiftUI's speculative width proposals.
        onOverflow?(lineMetrics().overflow)
    }
}
#endif
