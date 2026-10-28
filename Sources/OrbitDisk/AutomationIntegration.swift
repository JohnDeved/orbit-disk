import AppKit
import OrbitAutomation

@MainActor
enum AutomationIntegration {
    private static var working = false

    static func show() {
        guard !working else { present(message: "Setup is in progress", detail: "Wait for the current setup to finish."); return }
        let alert = NSAlert()
        alert.messageText = "Connect your AI assistant"
        alert.informativeText = "Let Codex use your maps and collection in a new chat. For another assistant, copy the MCP settings. Your assistant must ask you to approve the exact files before removing them."
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
            let sheet = alert.window
            let monitor = NSEvent.addLocalMonitorForEvents(matching: .leftMouseDown) { [weak window, weak sheet] event in
                guard let window, let sheet, window.attachedSheet === sheet,
                      sheet.attachedSheet == nil, event.window === window,
                      !sheet.frame.contains(window.convertPoint(toScreen: event.locationInWindow)) else { return event }
                window.endSheet(sheet, returnCode: .cancel)
                return nil
            }
            alert.beginSheetModal(for: window) { response in
                if let monitor { NSEvent.removeMonitor(monitor) }
                completion(response)
            }
        } else { completion(alert.runModal()) }
    }
}
