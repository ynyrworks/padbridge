import SwiftUI

@main
struct PadBridgeApp: App {
    @StateObject private var display = DisplayService()

    var body: some Scene {
        WindowGroup {
            ContentView()
                .environmentObject(display)
                .onAppear {
                    display.start()
                    // A display must not dim or lock while it's in use.
                    UIApplication.shared.isIdleTimerDisabled = true
                }
        }
    }
}
