import Foundation
import CoreGraphics

// Vision's enclosing rectangle can extend outside the image for edge text.
// Keep the visible portion without imposing an arbitrary overshoot threshold.
func visibleTextBounds(_ bounds: CGRect) throws -> CGRect? {
    guard [bounds.origin.x, bounds.origin.y, bounds.size.width, bounds.size.height].allSatisfy({ $0.isFinite }),
          bounds.size.width > 0, bounds.size.height > 0 else {
        throw NSError(domain: "PineFetchOCR", code: 4, userInfo: [NSLocalizedDescriptionKey: "Invalid Vision text bounding box."])
    }
    let visible = bounds.intersection(CGRect(x: 0, y: 0, width: 1, height: 1))
    guard !visible.isNull, !visible.isEmpty else { return nil }
    return visible
}
