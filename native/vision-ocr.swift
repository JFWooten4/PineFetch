import Foundation
import Vision
import ImageIO

// Protocol 1: one ready message, then one response per frame_id. stdout is JSONL only.
func emit(_ value: [String: Any]) throws {
    let data = try JSONSerialization.data(withJSONObject: value, options: [.sortedKeys])
    FileHandle.standardOutput.write(data)
    FileHandle.standardOutput.write(Data([10]))
}

@available(macOS 10.15, *)
func runVision() throws {
    let request = VNRecognizeTextRequest()
    request.recognitionLevel = .accurate
    request.usesLanguageCorrection = false
    request.minimumTextHeight = 0
    let supported = try VNRecognizeTextRequest.supportedRecognitionLanguages(for: .accurate, revision: request.revision)
    let desired = ["de-DE", "en-US"]
    var selected = desired.compactMap { desiredLanguage in
        supported.first(where: { $0 == desiredLanguage }) ?? supported.first(where: { $0.hasPrefix(String(desiredLanguage.prefix(2)) + "-") })
    }
    let fallback = selected.count != desired.count
    if selected.isEmpty {
        selected = request.recognitionLanguages.filter { supported.contains($0) }
        if selected.isEmpty, let first = supported.first { selected = [first] }
    }
    guard !selected.isEmpty else { throw NSError(domain: "PineFetchOCR", code: 1, userInfo: [NSLocalizedDescriptionKey: "Vision has no supported recognition languages."]) }
    request.recognitionLanguages = selected
    try emit([
        "type": "ready", "protocol_version": 1, "engine": "apple_vision",
        "engine_version": ["vision_request_revision": request.revision, "macos_version": ProcessInfo.processInfo.operatingSystemVersionString, "helper_version": "2"],
        "settings": ["recognition_level": "accurate", "recognition_languages": selected, "supported_languages": supported, "language_fallback": fallback, "uses_language_correction": false, "minimum_text_height": 0, "orientation": "up; FFmpeg autorotation", "coordinates": "normalized full oriented frame, bottom-left origin", "bounding_box_clipping": "intersection with normalized image rectangle"]
    ])
    while let line = readLine() {
        try autoreleasepool {
            guard line.utf8.count <= 65536,
                  let data = line.data(using: .utf8),
                  let input = try JSONSerialization.jsonObject(with: data) as? [String: Any],
                  let frameID = input["frame_id"] as? NSNumber,
                  let path = input["path"] as? String else {
                throw NSError(domain: "PineFetchOCR", code: 2, userInfo: [NSLocalizedDescriptionKey: "Invalid frame request."])
            }
            let handler = VNImageRequestHandler(url: URL(fileURLWithPath: path), orientation: .up, options: [:])
            try handler.perform([request])
            let observations: [[String: Any]] = try (request.results ?? []).compactMap { observation in
                guard let candidate = observation.topCandidates(1).first,
                      let bounds = try visibleTextBounds(observation.boundingBox) else { return nil }
                return ["text": candidate.string, "confidence": Double(candidate.confidence), "bounds": ["x": bounds.origin.x, "y": bounds.origin.y, "width": bounds.width, "height": bounds.height]]
            }
            try emit(["type": "frame", "frame_id": frameID, "observations": observations])
        }
    }
}

do {
    if #available(macOS 10.15, *) {
        try runVision()
    } else {
        throw NSError(domain: "PineFetchOCR", code: 3, userInfo: [NSLocalizedDescriptionKey: "Apple Vision OCR requires macOS 10.15 or later."])
    }
} catch {
    FileHandle.standardError.write(Data(("OCR helper: \(error.localizedDescription)\n").utf8))
    exit(1)
}
