import CoreGraphics

/// Maps view coordinates to the protocol's normalized video coordinates for
/// an aspect-fit video (letterboxed or pillarboxed inside the view).
public enum PointerMapping {
    /// Where an aspect-fit `videoSize` is drawn inside `bounds`.
    public static func contentRect(videoSize: CGSize, in bounds: CGRect) -> CGRect {
        guard videoSize.width > 0, videoSize.height > 0, bounds.width > 0, bounds.height > 0 else { return bounds }
        let scale = min(bounds.width / videoSize.width, bounds.height / videoSize.height)
        let size = CGSize(width: videoSize.width * scale, height: videoSize.height * scale)
        return CGRect(x: bounds.midX - size.width / 2, y: bounds.midY - size.height / 2,
                      width: size.width, height: size.height)
    }

    /// Normalized (0...1) position of `point` within the video. Points outside
    /// the video give nil, unless `clamp` is set (a drag that leaves the
    /// picture should stick to its edge rather than vanish).
    public static func normalize(_ point: CGPoint, videoSize: CGSize, in bounds: CGRect,
                                 clamp: Bool) -> (x: Float, y: Float)? {
        let rect = contentRect(videoSize: videoSize, in: bounds)
        guard rect.width > 0, rect.height > 0 else { return nil }
        var x = (point.x - rect.minX) / rect.width
        var y = (point.y - rect.minY) / rect.height
        if clamp {
            x = min(max(x, 0), 1)
            y = min(max(y, 0), 1)
        } else if !(0...1).contains(x) || !(0...1).contains(y) {
            return nil
        }
        return (Float(x), Float(y))
    }
}
