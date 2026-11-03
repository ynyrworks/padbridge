import SwiftUI

@main
struct PadBridgeApp: App {
    @StateObject private var display = DisplayService()
    @Environment(\.scenePhase) private var scenePhase

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
        .onChange(of: scenePhase) { _, phase in
            // iOS may tear the listener down while we're in the background;
            // the Windows side keeps retrying and gets back in once it's up again.
            if phase == .active { display.restartIfNeeded() }
        }
    }
}
