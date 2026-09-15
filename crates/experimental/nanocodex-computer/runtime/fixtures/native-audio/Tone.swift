import AppKit

final class ToneFixture: NSObject, NSApplicationDelegate {
    var window: NSWindow!
    var sound: NSSound?
    let status = NSTextField(labelWithString: "Ready. No sound is playing.")
    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.regular)
        let menu = NSMenu(), item = NSMenuItem(), submenu = NSMenu()
        submenu.addItem(withTitle: "Quit owned tone fixture", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        item.submenu = submenu; menu.addItem(item); NSApp.mainMenu = menu
        window = NSWindow(contentRect: NSRect(x:160,y:180,width:600,height:220), styleMask:[.titled,.closable,.miniaturizable], backing:.buffered, defer:false)
        window.title = "Skyre owned synthetic audio fixture"
        let label = NSTextField(labelWithString:"Only a quiet two-second generated 440 Hz tone. No microphone or system audio.")
        label.frame = NSRect(x:20,y:160,width:560,height:40); window.contentView!.addSubview(label)
        let play = NSButton(title:"Play synthetic tone", target:self, action:#selector(playTone))
        play.frame = NSRect(x:20,y:95,width:230,height:40); play.setAccessibilityIdentifier("skyre.audio.play"); window.contentView!.addSubview(play)
        let stop = NSButton(title:"Stop tone", target:self, action:#selector(stopTone))
        stop.frame = NSRect(x:300,y:95,width:160,height:40); stop.setAccessibilityIdentifier("skyre.audio.stop"); window.contentView!.addSubview(stop)
        status.frame = NSRect(x:20,y:30,width:560,height:40); status.setAccessibilityIdentifier("skyre.audio.status"); window.contentView!.addSubview(status)
        window.makeKeyAndOrderFront(nil); NSApp.activate(ignoringOtherApps:true)
    }
    @objc func playTone() {
        sound?.stop()
        let path = Bundle.main.url(forResource:"synthetic-440hz",withExtension:"wav")!
        sound = NSSound(contentsOf:path,byReference:false)
        sound?.volume = 0.15
        let playing = sound?.play() ?? false
        status.stringValue = playing ? "Playing owned 440 Hz tone (two-second maximum)." : "Tone playback failed."
        DispatchQueue.main.asyncAfter(deadline:.now()+2.1) { [weak self] in self?.stopTone() }
    }
    @objc func stopTone() { sound?.stop(); status.stringValue = "Stopped. No sound is playing." }
}
let app = NSApplication.shared
let delegate = ToneFixture()
app.delegate = delegate
app.run()
