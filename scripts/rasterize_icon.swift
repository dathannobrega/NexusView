// Rasterize an SVG into a macOS .iconset (all required sizes) using AppKit.
//   swift scripts/rasterize_icon.swift <input.svg> <output.iconset-dir>
import AppKit
import Foundation

let args = CommandLine.arguments
guard args.count >= 3 else {
    FileHandle.standardError.write(Data("usage: rasterize_icon <svg> <iconset-dir>\n".utf8))
    exit(1)
}
let svgURL = URL(fileURLWithPath: args[1])
let outDir = URL(fileURLWithPath: args[2])
try? FileManager.default.createDirectory(at: outDir, withIntermediateDirectories: true)

guard let image = NSImage(contentsOf: svgURL) else {
    FileHandle.standardError.write(Data("error: NSImage could not load the SVG\n".utf8))
    exit(2)
}

func render(_ pixels: Int, to url: URL) -> Bool {
    guard let rep = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: pixels, pixelsHigh: pixels,
        bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
        colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0
    ) else { return false }
    rep.size = NSSize(width: pixels, height: pixels)

    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
    NSGraphicsContext.current?.imageInterpolation = .high
    image.draw(in: NSRect(x: 0, y: 0, width: pixels, height: pixels),
               from: .zero, operation: .copy, fraction: 1.0)
    NSGraphicsContext.restoreGraphicsState()

    guard let data = rep.representation(using: .png, properties: [:]) else { return false }
    do { try data.write(to: url); return true } catch { return false }
}

let entries: [(String, Int)] = [
    ("icon_16x16.png", 16), ("icon_16x16@2x.png", 32),
    ("icon_32x32.png", 32), ("icon_32x32@2x.png", 64),
    ("icon_128x128.png", 128), ("icon_128x128@2x.png", 256),
    ("icon_256x256.png", 256), ("icon_256x256@2x.png", 512),
    ("icon_512x512.png", 512), ("icon_512x512@2x.png", 1024),
]
var ok = 0
for (name, px) in entries where render(px, to: outDir.appendingPathComponent(name)) { ok += 1 }
print("rendered \(ok)/\(entries.count) icons")
exit(ok == entries.count ? 0 : 3)
