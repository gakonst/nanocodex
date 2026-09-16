import CarPlay
import Combine
import InboxCore
import NanocodexVoice
import UIKit

@MainActor
final class NanocodexAppDelegate: NSObject, UIApplicationDelegate {
    func application(
        _ application: UIApplication,
        configurationForConnecting connectingSceneSession: UISceneSession,
        options: UIScene.ConnectionOptions
    ) -> UISceneConfiguration {
        if connectingSceneSession.role.rawValue == "CPTemplateApplicationSceneSessionRoleApplication" {
            let configuration = UISceneConfiguration(
                name: "NanocodexCarPlayScene",
                sessionRole: connectingSceneSession.role
            )
            configuration.sceneClass = CPTemplateApplicationScene.self
            configuration.delegateClass = CarPlaySceneDelegate.self
            return configuration
        }

        let configuration = UISceneConfiguration(
            name: "NanocodexDeviceScene",
            sessionRole: connectingSceneSession.role
        )
        configuration.sceneClass = UIWindowScene.self
        configuration.delegateClass = NanocodexDeviceSceneDelegate.self
        return configuration
    }
}

@MainActor
final class NanocodexDeviceSceneDelegate: NSObject, UIWindowSceneDelegate {
    func scene(_ scene: UIScene, openURLContexts URLContexts: Set<UIOpenURLContext>) {
        for context in URLContexts {
            Task { @MainActor in InboxModel.shared.handleURL(context.url) }
        }
    }
}

/// A voice-first CarPlay client for the same managed conversation used on the
/// phone. CarPlay owns no credentials or alternate agent transport; it drives
/// `VoiceSession` and renders only bounded call activity.
@MainActor
final class CarPlaySceneDelegate: UIResponder, CPTemplateApplicationSceneDelegate, CPInterfaceControllerDelegate {
    private let model = InboxModel.shared
    private var interfaceController: CPInterfaceController?
    private var rootTemplate: CPTabBarTemplate?
    private var voiceTab: CPListTemplate?
    private var agentsTab: CPListTemplate?
    private var voiceControlTemplate: CPVoiceControlTemplate?
    private var observations = Set<AnyCancellable>()
    private var activeAgentID: String?
    private var automaticStartConsumed = false
    private var connected = false

    @objc func templateApplicationScene(
        _ templateApplicationScene: CPTemplateApplicationScene,
        didConnect interfaceController: CPInterfaceController
    ) {
        connected = true
        self.interfaceController = interfaceController
        interfaceController.delegate = self
        model.setCarPlayConnected(true)

        let root = makeRootTemplate()
        rootTemplate = root
        interfaceController.setRootTemplate(root, animated: false, completion: nil)
        observeState()
        refreshPresentation()

        Task { @MainActor [weak self] in
            guard let self else { return }
            await self.model.start()
            self.refreshPresentation()
        }
    }

    @objc func templateApplicationScene(
        _ templateApplicationScene: CPTemplateApplicationScene,
        didDisconnect interfaceController: CPInterfaceController
    ) {
        connected = false
        observations.removeAll()
        model.voice.stop()
        model.setCarPlayConnected(false)
        voiceControlTemplate = nil
        rootTemplate = nil
        voiceTab = nil
        agentsTab = nil
        self.interfaceController = nil
    }

    func templateDidDisappear(_ aTemplate: CPTemplate, animated: Bool) {
        guard aTemplate === voiceControlTemplate else { return }
        voiceControlTemplate = nil
        model.voice.stop()
        refreshPresentation()
    }

    private func observeState() {
        observations.removeAll()
        let changed: () -> Void = { [weak self] in
            // @Published sends before committing the value. Read the complete
            // model on the next main-loop turn instead of projecting a mix of
            // old and new fields into CarPlay.
            DispatchQueue.main.async { self?.refreshPresentation() }
        }
        model.$cards.removeDuplicates().sink { _ in changed() }.store(in: &observations)
        model.$connected.removeDuplicates().sink { _ in changed() }.store(in: &observations)
        model.$restoringAccount.removeDuplicates().sink { _ in changed() }.store(in: &observations)
        model.$deck.removeDuplicates().sink { _ in changed() }.store(in: &observations)
        model.voice.$phase.removeDuplicates().sink { _ in changed() }.store(in: &observations)
        model.voice.$isMuted.removeDuplicates().sink { _ in changed() }.store(in: &observations)
        model.voice.$isReconnecting.removeDuplicates().sink { _ in changed() }.store(in: &observations)
        model.voice.$isWorking.removeDuplicates().sink { _ in changed() }.store(in: &observations)
        model.voice.$outputLevel.map { $0 > 0.015 }.removeDuplicates()
            .sink { _ in changed() }.store(in: &observations)
        model.voice.$conversationID.removeDuplicates().sink { _ in changed() }.store(in: &observations)
    }

    private func makeRootTemplate() -> CPTabBarTemplate {
        let voice = CPListTemplate(title: "Voice", sections: [voiceSection()])
        voice.tabImage = UIImage(systemName: "waveform.circle.fill")
        voiceTab = voice

        let agents = CPListTemplate(title: "Agents", sections: [agentsSection()])
        agents.tabImage = UIImage(systemName: "rectangle.stack.fill")
        agentsTab = agents

        return CPTabBarTemplate(templates: [voice, agents])
    }

    private func refreshPresentation() {
        guard connected else { return }
        voiceTab?.updateSections([voiceSection()])
        agentsTab?.updateSections([agentsSection()])

        if let template = voiceControlTemplate {
            template.activateVoiceControlState(withIdentifier: voiceStateIdentifier)
            updateVoiceButtons(template)
        }
        attemptAutomaticStart()
    }

    private func attemptAutomaticStart() {
        guard !automaticStartConsumed, !model.restoringAccount, model.connected else { return }
        if model.voice.isEngaged, let id = model.voice.conversationID {
            activeAgentID = id
            if presentVoiceControl() { automaticStartConsumed = true }
            return
        }
        guard let agent = selectedAgent ?? model.cards.sorted(by: AgentCard.mostRecentFirst).first else { return }
        beginVoice(agentID: agent.id)
    }

    private var selectedAgent: AgentCard? {
        let id = model.voice.conversationID ?? activeAgentID ?? model.focused?.id
        return id.flatMap { selected in model.cards.first { $0.id == selected } }
    }

    private func voiceSection() -> CPListSection {
        guard !model.restoringAccount else {
            return CPListSection(items: [disabledItem("Connecting to Nanocodex", detail: "Restoring your account")])
        }
        guard model.connected else {
            return CPListSection(items: [disabledItem("Open Nanocodex on iPhone", detail: "Sign in before using CarPlay")])
        }
        guard let agent = selectedAgent ?? model.cards.sorted(by: AgentCard.mostRecentFirst).first else {
            return CPListSection(items: [disabledItem("No conversations", detail: "Create one on your iPhone first")])
        }

        let start = CPListItem(
            text: model.voice.phase == .failed ? "Try voice again" : "Start voice",
            detailText: agent.title,
            image: UIImage(systemName: "mic.fill")
        )
        start.handler = { [weak self] _, completion in
            self?.beginVoice(agentID: agent.id)
            completion()
        }
        return CPListSection(items: [start])
    }

    private func agentsSection() -> CPListSection {
        let cards = model.cards.sorted(by: AgentCard.mostRecentFirst)
        guard !cards.isEmpty else {
            return CPListSection(items: [disabledItem("No conversations", detail: "Create one on your iPhone first")])
        }
        let selectedID = selectedAgent?.id
        let items = cards.map { card in
            let selected = card.id == selectedID
            let item = CPListItem(
                text: card.title,
                detailText: card.isRunning ? "Working" : card.status,
                image: UIImage(systemName: selected ? "checkmark.circle.fill" : "circle")
            )
            item.handler = { [weak self] _, completion in
                self?.beginVoice(agentID: card.id)
                completion()
            }
            return item
        }
        return CPListSection(items: items, header: "Conversations", sectionIndexTitle: nil)
    }

    private func disabledItem(_ text: String, detail: String) -> CPListItem {
        let item = CPListItem(text: text, detailText: detail, image: UIImage(systemName: "iphone"))
        item.isEnabled = false
        return item
    }

    private func beginVoice(agentID: String) {
        guard connected, model.connected else { return }
        if model.voice.isEngaged, model.voice.conversationID == agentID {
            _ = presentVoiceControl()
            return
        }
        guard presentVoiceControl() else { return }

        automaticStartConsumed = true
        activeAgentID = agentID
        model.select(agentID)
        model.voice.start(using: { [weak self] in
            guard let self else { throw CancellationError() }
            return try await self.model.voiceConfiguration(agentID: agentID)
        })
        refreshPresentation()
    }

    @discardableResult
    private func presentVoiceControl() -> Bool {
        guard let interfaceController else { return false }
        if let voiceControlTemplate {
            return interfaceController.presentedTemplate === voiceControlTemplate
        }
        guard interfaceController.presentedTemplate == nil else { return false }

        let template = CPVoiceControlTemplate(voiceControlStates: voiceStates)
        voiceControlTemplate = template
        updateVoiceButtons(template)
        template.activateVoiceControlState(withIdentifier: voiceStateIdentifier)
        interfaceController.presentTemplate(template, animated: true, completion: nil)
        return true
    }

    private var voiceStates: [CPVoiceControlState] {
        VoiceSession.Activity.allCases.map { activity in
            CPVoiceControlState(
                identifier: activity.rawValue,
                titleVariants: [title(for: activity)],
                image: UIImage(systemName: symbol(for: activity)),
                repeats: [.connecting, .listening, .reconnecting, .working].contains(activity)
            )
        }
    }

    private var voiceStateIdentifier: String { model.voice.activity.rawValue }

    private func title(for activity: VoiceSession.Activity) -> String {
        switch activity {
        case .ready: "Ready to talk"
        case .connecting: "Connecting…"
        case .listening: "Listening"
        case .reconnecting: "Reconnecting…"
        case .muted: "Microphone muted"
        case .speaking: "Speaking"
        case .working: "Working on it"
        case .failed: "Voice paused"
        }
    }

    private func symbol(for activity: VoiceSession.Activity) -> String {
        switch activity {
        case .ready: "mic.fill"
        case .connecting, .reconnecting: "ellipsis"
        case .listening: "waveform"
        case .muted: "mic.slash.fill"
        case .speaking: "speaker.wave.2.fill"
        case .working: "gearshape.2.fill"
        case .failed: "exclamationmark.triangle.fill"
        }
    }

    private func updateVoiceButtons(_ template: CPVoiceControlTemplate) {
        guard #available(iOS 26.4, *) else { return }
        let mute = CPBarButton(type: .text) { [weak self] _ in
            self?.model.voice.toggleMute()
        }
        mute.title = model.voice.isMuted ? "Unmute" : "Mute"

        let end = CPBarButton(type: .text) { [weak self] _ in
            self?.endVoice()
        }
        end.title = "End"
        template.trailingNavigationBarButtons = [mute, end]
    }

    private func endVoice() {
        model.voice.stop()
        if interfaceController?.presentedTemplate === voiceControlTemplate {
            interfaceController?.dismissTemplate(animated: true, completion: nil)
        }
        refreshPresentation()
    }
}
