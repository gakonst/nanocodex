import Foundation
let sources = ["", "abc", "e\u{301}Z", "🧪ab", "🇬🇷z", "👩‍🔬xy", "\r\nz", "\u{FEFF}ab"]
var rows: [[String: Any]] = []
for source in sources {
  for location in 0...(source.utf16.count + 1) {
    for length in 0...4 {
      let converted = Range(NSRange(location: location, length: length), in: source)
      let slice = converted.map { String(source[$0]) }
      rows.append(["source": source, "graphemes": source.count, "location": location,
                   "length": length, "slice": slice as Any? ?? NSNull()])
    }
  }
}
let data = try JSONSerialization.data(withJSONObject: rows, options: [.sortedKeys, .prettyPrinted])
FileHandle.standardOutput.write(data)
