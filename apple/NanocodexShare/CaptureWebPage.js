// Safari runs this only when the user chooses Nanocodex in the share sheet.
var ExtensionPreprocessingJS = {
    run: function (arguments) {
        var selection = String(window.getSelection() || "").trim();
        var content = document.querySelector("article, main, [role='main']") || document.body;
        var body = selection || (content ? content.innerText : "");
        var text = [document.title, body].filter(Boolean).join("\n\n").trim();
        arguments.completionFunction({ url: document.URL, text: text });
    }
};
