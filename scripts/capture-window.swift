// Capture one explicitly identified fixture window on macOS 14+.
// NSApplication initialization is required before creating the content filter.
import AppKit
import ScreenCaptureKit

let application = NSApplication.shared
guard CommandLine.arguments.count == 3,
      let windowID = UInt32(CommandLine.arguments[1]) else {
    fatalError("Usage: capture-window.swift WINDOW_ID OUTPUT.png")
}
let output = CommandLine.arguments[2]
let content = try await SCShareableContent.excludingDesktopWindows(false, onScreenWindowsOnly: true)
guard let window = content.windows.first(where: { $0.windowID == windowID }) else {
    fatalError("Dedicated window not found")
}
let filter = SCContentFilter(desktopIndependentWindow: window)
let config = SCStreamConfiguration()
config.width = Int(window.frame.width * 2)
config.height = Int(window.frame.height * 2)
config.showsCursor = false
let image: CGImage = try await withCheckedThrowingContinuation { continuation in
    SCScreenshotManager.captureImage(contentFilter: filter, configuration: config) { image, error in
        if let image = image { continuation.resume(returning: image) }
        else { continuation.resume(throwing: error ?? NSError(domain: "Capture", code: 1)) }
    }
}
let bitmap = NSBitmapImageRep(cgImage: image)
try bitmap.representation(using: .png, properties: [:])!.write(to: URL(fileURLWithPath: output))
