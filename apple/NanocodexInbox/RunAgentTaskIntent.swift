import AppIntents
import Foundation

struct HandAgentEntity: AppEntity {
    static var typeDisplayRepresentation = TypeDisplayRepresentation(name: "Agent")
    static var defaultQuery = HandAgentQuery()
    let agentID: String
    let account: String
    let title: String
    // A saved shortcut cannot silently follow a switch to another account.
    var id: String { account + ":" + agentID }
    var displayRepresentation: DisplayRepresentation { DisplayRepresentation(title: "\(title)") }
}

struct HandAgentQuery: EntityStringQuery {
    func entities(for identifiers: [String]) async throws -> [HandAgentEntity] {
        let agents = try await InboxModel.shared.shortcutAgents()
        return identifiers.compactMap { id in agents.first { $0.id == id } }
    }
    func suggestedEntities() async throws -> [HandAgentEntity] {
        try await InboxModel.shared.shortcutAgents()
    }
    func entities(matching string: String) async throws -> [HandAgentEntity] {
        try await suggestedEntities().filter { $0.title.localizedCaseInsensitiveContains(string) }
    }
}

struct RunAgentTaskIntent: ForegroundContinuableIntent {
    static var title: LocalizedStringResource = "Run Agent Task"
    static var description = IntentDescription("Send a request to an agent using this device's Hand. On iOS 27, work can finish in the background with progress and a Stop control. Earlier iOS versions open Nanocodex to start the task.")
    static var openAppWhenRun = false
    @available(iOS 26.0, macOS 26.0, *)
    static var supportedModes: IntentModes = [.background, .foreground(.dynamic)]
    @Parameter(title: "Agent") var agent: HandAgentEntity
    @Parameter(title: "Request") var request: String
    static var parameterSummary: some ParameterSummary { Summary("Ask \(\.$agent) to \(\.$request)") }

    @MainActor
    func perform() async throws -> some IntentResult & ReturnsValue<String> & ProvidesDialog {
        #if CENTAUR_APP_INTENTS_27
        if #available(iOS 27.0, macOS 27.0, *) {
            let invocation = HandIntentInvocation(agent: agent)
            let answer = try await performBackgroundTask {
                let work = try await InboxModel.shared.runShortcutTask(agent: agent, input: request, id: invocation.id,
                    progress: progress, runtimeProvided: true, isCancelled: { invocation.cancelled })
                return try await withTaskCancellationHandler {
                    try await work.value
                } onCancel: {
                    Task { @MainActor in invocation.cancel(stopTurn: false) }
                }
            } onCancel: { reason in
                Task { @MainActor in invocation.cancel(stopTurn: reason == .userCancelled) }
            }
            return .result(value: answer, dialog: "\(answer)")
        }
        #endif
        // iOS 26 grants continued processing only for a foreground action.
        // Return after queueing; the app owns the continuing task from here.
        #if os(iOS)
        try await requestToContinueInForeground()
        #endif
        let id = UUID().uuidString
        _ = try await InboxModel.shared.runShortcutTask(agent: agent, input: request, id: id)
        return .result(value: "Task started in Nanocodex.", dialog: "Task started in Nanocodex.")
    }
}

#if CENTAUR_APP_INTENTS_27
@available(iOS 27.0, macOS 27.0, *)
extension RunAgentTaskIntent: LongRunningIntent, CancellableIntent {
    static var allowedExecutionTargets: IntentExecutionTargets { .main }
}

@MainActor
private final class HandIntentInvocation {
    let id = UUID().uuidString
    let agent: HandAgentEntity
    private(set) var cancelled = false
    init(agent: HandAgentEntity) { self.agent = agent }
    func cancel(stopTurn: Bool) {
        cancelled = true
        InboxModel.shared.cancelShortcutTask(id: id, agent: agent, stopTurn: stopTurn)
    }
}
#endif

enum HandTaskError: LocalizedError {
    case signIn, disabled, accountChanged, emptyRequest, delivery(String)
    var errorDescription: String? {
        switch self {
        case .signIn: "Open Nanocodex and sign in before running this shortcut."
        case .disabled: "This device's Hand is disabled. Enable it in Nanocodex Settings to run this task."
        case .accountChanged: "This shortcut's agent belongs to a different account. Choose an agent from the connected account."
        case .emptyRequest: "Enter a request for the agent."
        case .delivery(let message): message
        }
    }
}
