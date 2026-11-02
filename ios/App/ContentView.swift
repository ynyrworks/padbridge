import ReplayKit
import SwiftUI

struct ContentView: View {
    @EnvironmentObject private var display: DisplayService

    var body: some View {
        NavigationStack {
            List {
                Section("Display 模式（Windows → iPad）") {
                    LabeledContent("狀態", value: display.status)
                    LabeledContent("已連線", value: display.peers.isEmpty ? "—" : display.peers.joined(separator: ", "))
                }
                Section {
                    HStack {
                        Text("開始 / 停止廣播")
                        Spacer()
                        BroadcastPicker().frame(width: 44, height: 44)
                    }
                } header: {
                    Text("Mirror 模式（iPad → Windows）")
                } footer: {
                    Text("廣播開始後，Windows 端連線至埠 47801。")
                }
                Section("事件") {
                    ForEach(display.log) { line in
                        VStack(alignment: .leading) {
                            Text(line.text).font(.callout.monospaced())
                            Text(line.time, style: .time).font(.caption).foregroundStyle(.secondary)
                        }
                    }
                }
            }
            .navigationTitle("PadBridge")
        }
    }
}

/// System broadcast picker preselecting our upload extension.
struct BroadcastPicker: UIViewRepresentable {
    func makeUIView(context: Context) -> RPSystemBroadcastPickerView {
        let picker = RPSystemBroadcastPickerView()
        picker.preferredExtension = Bundle.main.bundleIdentifier.map { "\($0).broadcast" }
        picker.showsMicrophoneButton = false
        return picker
    }

    func updateUIView(_ uiView: RPSystemBroadcastPickerView, context: Context) {}
}
