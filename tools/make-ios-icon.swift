// Flattens assets/icon-1024.png onto an opaque background for the iOS app icon
// (App Store Connect rejects icons with an alpha channel; iOS rounds the corners
// itself). Run with: swift tools/make-ios-icon.swift
import CoreGraphics
import Foundation
import ImageIO
import UniformTypeIdentifiers

let root = URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
let input = root.appendingPathComponent("assets/icon-1024.png")
let output = root.appendingPathComponent(
    "apps/ios/VNCBeach/Resources/Assets.xcassets/AppIcon.appiconset/AppIcon-1024.png")

guard let source = CGImageSourceCreateWithURL(input as CFURL, nil),
    let image = CGImageSourceCreateImageAtIndex(source, 0, nil)
else {
    fatalError("cannot read \(input.path)")
}
let size = 1024
let space = CGColorSpace(name: CGColorSpace.sRGB)!
let context = CGContext(
    data: nil, width: size, height: size, bitsPerComponent: 8, bytesPerRow: size * 4, space: space,
    bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue)!
// Sky colour from assets/icon.svg, so transparent corners blend in.
context.setFillColor(CGColor(colorSpace: space, components: [0x8e / 255.0, 0xdd / 255.0, 0xfb / 255.0, 1])!)
context.fill(CGRect(x: 0, y: 0, width: size, height: size))
context.draw(image, in: CGRect(x: 0, y: 0, width: size, height: size))
let flat = context.makeImage()!
let dest = CGImageDestinationCreateWithURL(output as CFURL, UTType.png.identifier as CFString, 1, nil)!
CGImageDestinationAddImage(dest, flat, nil)
guard CGImageDestinationFinalize(dest) else { fatalError("cannot write \(output.path)") }
print("wrote \(output.path)")
