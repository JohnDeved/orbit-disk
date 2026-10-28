import AppKit
import OrbitAutomation

@MainActor
enum AutomationIntegration {
    private static var working = false

    static func show() {
        guard !working else { present(message: "Setup is in progress", detail: "Wait for the current setup to finish."); return }
        let alert = NSAlert()
        alert.messageText = "Use Orbit Disk with AI tools"
        alert.informativeText = "Connect once so Codex can discover Orbit Disk’s tools in new conversations. It can inspect maps and manage your collection. Removing files requires your approval of the exact items and removal mode."
        alert.addButton(withTitle: "Connect to Codex")
        alert.addButton(withTitle: "Copy MCP Settings")
        alert.addButton(withTitle: "Install Command Line Tool")
        alert.addButton(withTitle: "Cancel")
        show(alert) { response in
            let control = Bundle.main.bundleURL.appendingPathComponent("Contents/MacOS/OrbitControl")
            let installer = IntegrationInstaller(controlURL: control)
            if response == .alertSecondButtonReturn {
                do {
                    let settings = try installer.mcpSettings()
                    NSPasteboard.general.clearContents()
                    NSPasteboard.general.setString(settings, forType: .string)
                    present(message: "MCP settings copied", detail: "Add the orbit-disk entry to your AI client’s MCP settings, then reconnect that client.")
                } catch { present(message: "Couldn’t copy settings", detail: error.localizedDescription) }
            } else if response == .alertFirstButtonReturn || response.rawValue == NSApplication.ModalResponse.alertThirdButtonReturn.rawValue {
                let connect = response == .alertFirstButtonReturn
                working = true
                Task {
                    let result = await Task.detached(priority: .userInitiated) {
                        Result { try connect ? installer.connectCodex() : installer.installCommandLineTool() }
                    }.value
                    working = false
                    switch result {
                    case .success(let message): present(message: connect ? "Orbit Disk is connected" : "Command line tool installed", detail: message)
                    case .failure(let error): present(message: "Setup couldn’t finish", detail: error.localizedDescription)
                    }
                }
            }
        }
    }

    private static func present(message: String, detail: String) {
        let alert = NSAlert(); alert.messageText = message; alert.informativeText = detail
        alert.addButton(withTitle: "OK"); show(alert) { _ in }
    }

    private static func show(_ alert: NSAlert, completion: @escaping @MainActor (NSApplication.ModalResponse) -> Void) {
        if let window = NSApp.keyWindow ?? NSApp.mainWindow {
            alert.beginSheetModal(for: window) { response in completion(response) }
        } else { completion(alert.runModal()) }
    }
}
