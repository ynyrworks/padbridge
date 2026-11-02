import AVFoundation
import ReplayKit
import SwiftUI

struct ContentView: View {
    @EnvironmentObject private var display: DisplayService

    var body: some View {
        ZStack {
            NavigationStack {
                List {
                    Section {
                        LabeledContent("狀態", value: display.status)
                        LabeledContent("已連線", value: display.peers.isEmpty ? "—" : display.peers.joined(separator: ", "))
                    } header: {
                        Text("Display 模式（Windows → iPad）")
                    } footer: {
                        Text("在 Windows 的 PadBridge 按「延伸螢幕」，畫面就會全螢幕顯示在這裡。")
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

            if display.streamInfo != nil {
                VideoLayerView(layer: display.receiver.layer)
                    .ignoresSafeArea()
                    .transition(.opacity)
            }
        }
        .animation(.easeInOut(duration: 0.2), value: display.streamInfo != nil)
        .statusBarHidden(display.streamInfo != nil)
        .persistentSystemOverlays(display.streamInfo != nil ? .hidden : .automatic)
    }
}

/// Hosts the receiver's display layer, sized to fill the view.
struct VideoLayerView: UIViewRepresentable {
    let layer: AVSampleBufferDisplayLayer

    final class HostView: UIView {
        var videoLayer: CALayer? {
            didSet {
                oldValue?.removeFromSuperlayer()
                if let videoLayer { layer.addSublayer(videoLayer) }
            }
        }

        override func layoutSubviews() {
            super.layoutSubviews()
            CATransaction.begin()
            CATransaction.setDisableActions(true)
            videoLayer?.frame = bounds
            CATransaction.commit()
        }
    }

    func makeUIView(context: Context) -> HostView {
        let view = HostView()
        view.backgroundColor = .black
        view.videoLayer = layer
        return view
    }

    func updateUIView(_ view: HostView, context: Context) {
        if view.videoLayer !== layer { view.videoLayer = layer }
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
