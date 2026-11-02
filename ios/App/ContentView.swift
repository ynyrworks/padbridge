import AVFoundation
import PadBridgeMedia
import PadBridgeProtocol
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
                VideoLayerView(layer: display.receiver.layer, cursor: display.cursorLayer, videoSize: display.streamSize) { message in
                    display.sendPointer(message)
                }
                .ignoresSafeArea()
                .defersSystemGestures(on: .all)
                .transition(.opacity)
            }
        }
        .animation(.easeInOut(duration: 0.2), value: display.streamInfo != nil)
        .statusBarHidden(display.streamInfo != nil)
        .persistentSystemOverlays(display.streamInfo != nil ? .hidden : .automatic)
    }
}

/// The host's mouse cursor, drawn over the video (the host does not render it
/// into frames, so moving it costs no bandwidth). Its tip is the hot spot.
final class CursorLayer: CAShapeLayer {
    /// Where the video is drawn inside the view; set by the hosting view.
    var contentRect = CGRect.zero { didSet { reposition() } }
    private var normalized = CGPoint.zero

    override init() {
        super.init()
        let arrow = CGMutablePath()
        arrow.addLines(between: [CGPoint(x: 0, y: 0), CGPoint(x: 0, y: 17), CGPoint(x: 4.5, y: 13),
                                 CGPoint(x: 7.5, y: 19.5), CGPoint(x: 10, y: 18.5), CGPoint(x: 7, y: 12),
                                 CGPoint(x: 12.5, y: 12)])
        arrow.closeSubpath()
        path = arrow
        fillColor = UIColor.white.cgColor
        strokeColor = UIColor.black.cgColor
        lineWidth = 1.2
        anchorPoint = .zero
        bounds = CGRect(x: 0, y: 0, width: 13, height: 20)
        zPosition = 10
        isHidden = true
    }

    override init(layer: Any) { super.init(layer: layer) }

    required init?(coder: NSCoder) { fatalError("not used") }

    /// Call on the main thread.
    func update(x: Float, y: Float, visible: Bool) {
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        normalized = CGPoint(x: CGFloat(x), y: CGFloat(y))
        isHidden = !visible
        reposition()
        CATransaction.commit()
    }

    private func reposition() {
        position = CGPoint(x: contentRect.minX + normalized.x * contentRect.width,
                           y: contentRect.minY + normalized.y * contentRect.height)
    }
}

/// Hosts the receiver's display layer, sized to fill the view, and turns
/// touches, Apple Pencil and trackpad input on it into `Pointer` messages.
struct VideoLayerView: UIViewRepresentable {
    let layer: AVSampleBufferDisplayLayer
    let cursor: CursorLayer
    let videoSize: CGSize
    let onPointer: (Message) -> Void

    final class HostView: UIView {
        var videoLayer: CALayer? {
            didSet {
                oldValue?.removeFromSuperlayer()
                if let videoLayer { layer.insertSublayer(videoLayer, at: 0) }
            }
        }
        var cursorLayer: CursorLayer? {
            didSet {
                oldValue?.removeFromSuperlayer()
                if let cursorLayer { layer.addSublayer(cursorLayer) }
            }
        }
        var videoSize: CGSize = .zero {
            didSet { setNeedsLayout() }
        }
        var onPointer: ((Message) -> Void)?

        /// Protocol ids for the touches currently down.
        private var ids: [ObjectIdentifier: UInt32] = [:]
        private var nextId: UInt32 = 1
        private var penHovering = false

        override init(frame: CGRect) {
            super.init(frame: frame)
            isMultipleTouchEnabled = true
            addGestureRecognizer(UIHoverGestureRecognizer(target: self, action: #selector(hover(_:))))
        }

        required init?(coder: NSCoder) { fatalError("not used") }

        override func layoutSubviews() {
            super.layoutSubviews()
            CATransaction.begin()
            CATransaction.setDisableActions(true)
            videoLayer?.frame = bounds
            cursorLayer?.contentRect = PointerMapping.contentRect(videoSize: videoSize, in: bounds)
            CATransaction.commit()
        }

        override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent?) {
            for touch in touches { send(touch, .down) }
        }

        override func touchesMoved(_ touches: Set<UITouch>, with event: UIEvent?) {
            // Coalesced touches carry the full 120–240 Hz Pencil sample rate.
            for touch in touches {
                for sample in event?.coalescedTouches(for: touch) ?? [touch] { send(touch, .move, sample: sample) }
            }
        }

        override func touchesEnded(_ touches: Set<UITouch>, with event: UIEvent?) {
            for touch in touches { send(touch, .up) }
        }

        override func touchesCancelled(_ touches: Set<UITouch>, with event: UIEvent?) {
            for touch in touches { send(touch, .cancel) }
        }

        private func send(_ touch: UITouch, _ phase: Phase, sample: UITouch? = nil) {
            let key = ObjectIdentifier(touch)
            if phase == .down {
                ids[key] = nextId
                nextId &+= 1
            }
            guard let id = ids[key] else { return }
            if phase == .up || phase == .cancel { ids[key] = nil }

            let s = sample ?? touch
            // A new touch in the black bars is ignored; a drag leaving the picture sticks to its edge.
            guard let point = PointerMapping.normalize(s.location(in: self), videoSize: videoSize, in: bounds,
                                                       clamp: phase != .down) else {
                ids[key] = nil
                return
            }
            let kind: PointerKind = switch touch.type {
            case .pencil: .pen
            case .indirectPointer: .mouse
            default: .touch
            }
            let pressure = s.maximumPossibleForce > 0 ? Float(s.force / s.maximumPossibleForce) : 0
            onPointer?(.pointer(id: id, kind: kind, phase: phase, x: point.x, y: point.y, pressure: pressure,
                                altitude: Float(s.altitudeAngle), azimuth: Float(s.azimuthAngle(in: self))))
        }

        /// Apple Pencil hover (supported models) or the trackpad pointer moving without a click.
        @objc private func hover(_ recognizer: UIHoverGestureRecognizer) {
            let isPen = recognizer.zOffset > 0 || penHovering
            let kind: PointerKind = isPen ? .pen : .mouse
            let phase: Phase
            switch recognizer.state {
            case .began, .changed:
                phase = .hover
                penHovering = recognizer.zOffset > 0
            case .ended, .cancelled:
                // Only the pen has a "left range" state on Windows.
                guard penHovering else { return }
                phase = .cancel
                penHovering = false
            default:
                return
            }
            guard let point = PointerMapping.normalize(recognizer.location(in: self), videoSize: videoSize,
                                                       in: bounds, clamp: true) else { return }
            onPointer?(.pointer(id: 0, kind: kind, phase: phase, x: point.x, y: point.y, pressure: 0,
                                altitude: Float(recognizer.altitudeAngle),
                                azimuth: Float(recognizer.azimuthAngle(in: self))))
        }
    }

    func makeUIView(context: Context) -> HostView {
        let view = HostView()
        view.backgroundColor = .black
        view.videoLayer = layer
        view.cursorLayer = cursor
        return view
    }

    func updateUIView(_ view: HostView, context: Context) {
        if view.videoLayer !== layer { view.videoLayer = layer }
        if view.cursorLayer !== cursor { view.cursorLayer = cursor }
        view.videoSize = videoSize
        view.onPointer = onPointer
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
