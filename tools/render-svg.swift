// Renders an SVG to a PNG with a transparent background at an exact pixel size
// (QuickLook's qlmanage flattens SVG thumbnails onto white).
// Usage: swift tools/render-svg.swift in.svg out.png WIDTH HEIGHT
import AppKit

let args = CommandLine.arguments
guard args.count == 5, let width = Int(args[3]), let height = Int(args[4]),
    let image = NSImage(contentsOfFile: args[1])
else {
    FileHandle.standardError.write("usage: render-svg.swift in.svg out.png WIDTH HEIGHT\n".data(using: .utf8)!)
    exit(2)
}
let rep = NSBitmapImageRep(
    bitmapDataPlanes: nil, pixelsWide: width, pixelsHigh: height, bitsPerSample: 8, samplesPerPixel: 4,
    hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
rep.size = NSSize(width: width, height: height)
NSGraphicsContext.saveGraphicsState()
let context = NSGraphicsContext(bitmapImageRep: rep)!
NSGraphicsContext.current = context
context.cgContext.interpolationQuality = .high
image.draw(in: NSRect(x: 0, y: 0, width: width, height: height), from: .zero, operation: .sourceOver, fraction: 1)
NSGraphicsContext.restoreGraphicsState()
guard let png = rep.representation(using: .png, properties: [:]) else { exit(1) }
try! png.write(to: URL(fileURLWithPath: args[2]))
