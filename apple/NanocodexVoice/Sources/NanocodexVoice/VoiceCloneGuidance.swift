import Foundation

enum VoiceCloneGuidance {
    static func canCreate(name: String, count: Int, consent: Bool) -> Bool {
        consent && !name.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty && name.count <= 100 && (1...5).contains(count)
    }
    static let script = """
    This morning, I opened the window and listened to the neighborhood waking up. A bicycle rolled past, someone called a friendly greeting, and the leaves moved gently in the breeze. I decided to take a short walk before starting the day.

    At the corner, a small shop was arranging fresh flowers beside the door. The colors reminded me of a garden I used to visit, where every path seemed to lead somewhere different. I paused for a moment, then continued toward the park.

    There was no need to hurry. I thought about the work ahead, the people I wanted to call, and a meal I might cook that evening. Ordinary plans can be surprisingly comforting. By the time I returned home, I felt ready to begin, with a clear mind and a little more patience for whatever the day might bring.
    """
}
