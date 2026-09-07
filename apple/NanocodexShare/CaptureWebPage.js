// Safari runs this only when the user chooses Nanocodex in the share sheet.
var ExtensionPreprocessingJS = {
    run: function (arguments) {
        var selection = String(window.getSelection() || "").trim();
        var content = document.querySelector("article, main, [role='main']") || document.body;
        var body = selection || (content ? content.innerText : "");
        var text = [document.title, body].filter(Boolean).join("\n\n").trim();
        var encoder = new TextEncoder();
        var limit = 24 * 1024;
        if (encoder.encode(text).length > limit) {
            var suffix = "\n\n[Excerpt truncated]";
            var bytes = 0;
            var excerpt = "";
            for (var character of text) {
                bytes += encoder.encode(character).length;
                if (bytes > limit - encoder.encode(suffix).length) break;
                excerpt += character;
            }
            text = excerpt + suffix;
        }
        arguments.completionFunction({ url: document.URL, text: text });
    }
};
