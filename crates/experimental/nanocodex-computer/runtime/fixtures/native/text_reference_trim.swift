// Foundation-only oracle matching the calls recovered in 100740fd0.
// This creates no app, opens no resource and performs no accessibility request.
import Foundation
let samples = [
    " \tβ🧪\n ", " \u{301}x ", "\u{200b}α\u{200b}",
    "\u{00a0}x\u{00a0}", "\u{2028}\tx\n", "   ",
    "\t[e\u{301}](x) \n", "\u{3000}\u{301}🧪\u{3000}",
    "\r\nα\u{85}", "\u{feff}x\u{feff}"
]
let rows: [[String: Any]] = samples.map { source in
    let trimmed = source.trimmingCharacters(in: .whitespacesAndNewlines)
    let range = (source as NSString).range(of: trimmed)
    return ["source": source, "trimmed": trimmed,
            "location": range.location == NSNotFound ? NSNull() : range.location,
            "length": range.length]
}
let data = try JSONSerialization.data(withJSONObject: rows, options: [.sortedKeys, .prettyPrinted])
FileHandle.standardOutput.write(data)
FileHandle.standardOutput.write(Data([10]))
