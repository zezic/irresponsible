#!/usr/bin/env swift
import AppKit
import Foundation

let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
let assets = root.appendingPathComponent("assets/app-icon", isDirectory: true)
let source = try String(contentsOf: assets.appendingPathComponent("icon.svg"), encoding: .utf8)
let temporary = FileManager.default.temporaryDirectory.appendingPathComponent("irresponsible-icon-\(UUID().uuidString)", isDirectory: true)
let iconset = temporary.appendingPathComponent("AppIcon.iconset", isDirectory: true)
try FileManager.default.createDirectory(at: iconset, withIntermediateDirectories: true)

enum IconError: Error {
    case sourceImage, bitmapContext, bitmapImage, pngEncoding, iconutil(Int32)
}

func png(logicalSize: Int, scale: Int) throws -> Data {
    // Keep the same mark readable at one logical pixel in the smallest sizes.
    let stroke = max(30.0, 1024.0 / Double(logicalSize))
    let opticalSource = source.replacingOccurrences(of: "stroke-width=\"30\"", with: "stroke-width=\"\(stroke)\"")
    guard let image = NSImage(data: Data(opticalSource.utf8)) else { throw IconError.sourceImage }
    let pixels = logicalSize * scale
    guard let context = CGContext(data: nil, width: pixels, height: pixels, bitsPerComponent: 8,
        bytesPerRow: 0, space: CGColorSpace(name: CGColorSpace.sRGB)!,
        bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { throw IconError.bitmapContext }
    context.clear(CGRect(x: 0, y: 0, width: pixels, height: pixels))
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(cgContext: context, flipped: false)
    NSGraphicsContext.current!.imageInterpolation = .high
    image.draw(in: NSRect(x: 0, y: 0, width: pixels, height: pixels), from: .zero, operation: .copy, fraction: 1)
    NSGraphicsContext.restoreGraphicsState()
    guard let rendered = context.makeImage() else { throw IconError.bitmapImage }
    guard let data = NSBitmapImageRep(cgImage: rendered).representation(using: .png, properties: [:]) else { throw IconError.pngEncoding }
    return data
}

for size in [16, 32, 128, 256, 512] {
    for scale in [1, 2] {
        let suffix = scale == 2 ? "@2x" : ""
        let data = try png(logicalSize: size, scale: scale)
        try data.write(to: iconset.appendingPathComponent("icon_\(size)x\(size)\(suffix).png"))
        if size == 512 && scale == 2 {
            try data.write(to: temporary.appendingPathComponent("AppIcon.png"))
        }
    }
}
let task = Process()
task.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
task.arguments = ["-c", "icns", iconset.path, "-o", temporary.appendingPathComponent("AppIcon.icns").path]
try task.run()
task.waitUntilExit()
guard task.terminationStatus == 0 else { throw IconError.iconutil(task.terminationStatus) }
for name in ["AppIcon.png", "AppIcon.icns"] {
    try Data(contentsOf: temporary.appendingPathComponent(name)).write(to: assets.appendingPathComponent(name), options: .atomic)
}
try FileManager.default.removeItem(at: temporary)
print("Updated assets/app-icon/AppIcon.png and AppIcon.icns")
