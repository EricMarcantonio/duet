#!/usr/bin/env swift
// Generates the Duet app icon entirely locally with CoreGraphics.
// Usage: swift scripts/make-icon.swift   (run from the repo root)
// Outputs: assets/icon/{icon-1024,32x32,128x128,256x256}.png, Duet.iconset/, Duet.icns, icon.ico
import Foundation
import CoreGraphics
import ImageIO
import UniformTypeIdentifiers

let root = URL(fileURLWithPath: FileManager.default.currentDirectoryPath)
let outDir = root.appendingPathComponent("assets/icon")
let iconsetDir = outDir.appendingPathComponent("Duet.iconset")
try? FileManager.default.removeItem(at: iconsetDir)
try! FileManager.default.createDirectory(at: iconsetDir, withIntermediateDirectories: true)

let cs = CGColorSpace(name: CGColorSpace.sRGB)!
func rgb(_ hex: UInt32, _ a: CGFloat = 1) -> CGColor {
    CGColor(colorSpace: cs, components: [CGFloat((hex >> 16) & 0xFF) / 255,
                                         CGFloat((hex >> 8) & 0xFF) / 255,
                                         CGFloat(hex & 0xFF) / 255, a])!
}

// Superellipse "squircle" approximating Apple's continuous-corner icon shape.
func squircle(_ r: CGRect, n: CGFloat = 4.8) -> CGPath {
    let p = CGMutablePath()
    let a = r.width / 2, b = r.height / 2, cx = r.midX, cy = r.midY
    let steps = 720
    for i in 0...steps {
        let t = CGFloat(i) / CGFloat(steps) * 2 * .pi
        let c = cos(t), s = sin(t)
        let x = cx + a * (c < 0 ? -1 : 1) * pow(abs(c), 2 / n)
        let y = cy + b * (s < 0 ? -1 : 1) * pow(abs(s), 2 / n)
        if i == 0 { p.move(to: CGPoint(x: x, y: y)) } else { p.addLine(to: CGPoint(x: x, y: y)) }
    }
    p.closeSubpath()
    return p
}

// Headphone silhouette as a filled path, in a unit space where the band radius = 1.
// Coordinates: y up. Center of band arc at origin.
func headphones(center: CGPoint, scale s: CGFloat, bold: Bool) -> [CGPath] {
    var parts: [CGPath] = []
    let bandW: CGFloat = bold ? 0.40 : 0.22      // headband stroke thickness
    let r: CGFloat = 1.0                          // band centerline radius
    // Headband: arc from left to right over the top, continuing down to cups.
    let band = CGMutablePath()
    band.move(to: CGPoint(x: -r, y: -0.30))
    band.addLine(to: CGPoint(x: -r, y: 0))
    band.addArc(center: .zero, radius: r, startAngle: .pi, endAngle: 0, clockwise: true)
    band.addLine(to: CGPoint(x: r, y: -0.30))
    let bandStroke = band.copy(strokingWithWidth: bandW, lineCap: .round, lineJoin: .round, miterLimit: 10)
    parts.append(bandStroke)
    // Ear cups: rounded rects hugging the band ends.
    let cupW: CGFloat = bold ? 0.72 : 0.52, cupH: CGFloat = bold ? 0.95 : 0.82
    for side in [-1.0, 1.0] as [CGFloat] {
        let cx = side * (r - 0.02) - side * 0.03
        let rect = CGRect(x: cx - cupW / 2, y: -0.20 - cupH, width: cupW, height: cupH)
        parts.append(CGPath(roundedRect: rect, cornerWidth: cupW * 0.42, cornerHeight: cupW * 0.42, transform: nil))
    }
    var t = CGAffineTransform(translationX: center.x, y: center.y).scaledBy(x: s, y: s)
    return parts.map { $0.copy(using: &t)! }
}

// Fill each part separately so overlaps union cleanly (no winding holes).
func fill(_ ctx: CGContext, _ parts: [CGPath]) { for p in parts { ctx.addPath(p); ctx.fillPath() } }

func render(_ px: Int) -> CGImage {
    let S = CGFloat(px)
    let ctx = CGContext(data: nil, width: px, height: px, bitsPerComponent: 8, bytesPerRow: 0,
                        space: cs, bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)!
    ctx.interpolationQuality = .high
    ctx.setShouldAntialias(true)
    let k = S / 1024                         // design units -> pixels
    let small = px <= 32
    ctx.scaleBy(x: k, y: k)

    // Icon body on the standard macOS grid: 824x824 at (100,100); slight shadow below.
    let body = CGRect(x: 100, y: 100, width: 824, height: 824)
    let shape = squircle(body)

    if !small {
        ctx.saveGState()
        ctx.setShadow(offset: CGSize(width: 0, height: -10 * k), blur: 28 * k, color: rgb(0x000000, 0.30))
        ctx.addPath(shape); ctx.setFillColor(rgb(0x2A3BD8)); ctx.fillPath()
        ctx.restoreGState()
    }

    // Background gradient: bright blue (top) -> indigo -> violet (bottom).
    ctx.saveGState()
    ctx.addPath(shape); ctx.clip()
    let grad = CGGradient(colorsSpace: cs,
                          colors: [rgb(0x2FA6FF), rgb(0x0A84FF), rgb(0x4B4FE6), rgb(0x7B3FE0)] as CFArray,
                          locations: [0, 0.30, 0.68, 1])!
    ctx.drawLinearGradient(grad, start: CGPoint(x: 330, y: 924), end: CGPoint(x: 694, y: 100), options: [.drawsBeforeStartLocation, .drawsAfterEndLocation])
    // Soft top highlight for depth.
    let hl = CGGradient(colorsSpace: cs, colors: [rgb(0xFFFFFF, 0.22), rgb(0xFFFFFF, 0)] as CFArray, locations: [0, 1])!
    ctx.drawRadialGradient(hl, startCenter: CGPoint(x: 512, y: 960), startRadius: 0,
                           endCenter: CGPoint(x: 512, y: 960), endRadius: 620, options: [])
    // Subtle inner edge line.
    ctx.addPath(shape); ctx.setStrokeColor(rgb(0xFFFFFF, small ? 0 : 0.14)); ctx.setLineWidth(4); ctx.strokePath()
    ctx.restoreGState()

    // Glyph: two headphones — one behind (tinted, smaller), one in front (white).
    let bold = small
    let backC = small ? CGPoint(x: 380, y: 610) : CGPoint(x: 405, y: 590)
    let frontC = small ? CGPoint(x: 610, y: 450) : CGPoint(x: 600, y: 470)
    let backS: CGFloat = small ? 170 : 165
    let frontS: CGFloat = small ? 215 : 200
    let back = headphones(center: backC, scale: backS, bold: bold)
    let front = headphones(center: frontC, scale: frontS, bold: bold)

    // Draw glyphs into a layer so we can knock a gap around the front headphones.
    ctx.saveGState()
    ctx.setShadow(offset: CGSize(width: 0, height: -8 * k), blur: 24 * k, color: rgb(0x1A0F6B, small ? 0 : 0.35))
    ctx.beginTransparencyLayer(auxiliaryInfo: nil)
    // Back headphones, semi-opaque, in their own layer so overlapping parts don't double up.
    ctx.saveGState(); ctx.setAlpha(small ? 0.62 : 0.72)
    ctx.beginTransparencyLayer(auxiliaryInfo: nil)
    ctx.setFillColor(rgb(0xD6EEFF)); fill(ctx, back)
    ctx.endTransparencyLayer(); ctx.restoreGState()
    // Knockout ring around the front glyph.
    ctx.setBlendMode(.clear)
    for p in front {
        ctx.addPath(p.copy(strokingWithWidth: small ? 56 : 44, lineCap: .round, lineJoin: .round, miterLimit: 10)); ctx.fillPath()
    }
    fill(ctx, front)
    ctx.setBlendMode(.normal)
    ctx.setFillColor(rgb(0xFFFFFF)); fill(ctx, front)
    ctx.endTransparencyLayer()
    ctx.restoreGState()

    return ctx.makeImage()!
}

func pngData(_ img: CGImage) -> Data {
    let d = NSMutableData()
    let dest = CGImageDestinationCreateWithData(d, UTType.png.identifier as CFString, 1, nil)!
    CGImageDestinationAddImage(dest, img, nil)
    CGImageDestinationFinalize(dest)
    return d as Data
}
func write(_ data: Data, _ url: URL) { try! data.write(to: url) }

var cache: [Int: Data] = [:]
func png(_ px: Int) -> Data {
    if let d = cache[px] { return d }
    let d = pngData(render(px)); cache[px] = d; return d
}

// Iconset (macOS naming).
for base in [16, 32, 128, 256, 512] {
    write(png(base), iconsetDir.appendingPathComponent("icon_\(base)x\(base).png"))
    write(png(base * 2), iconsetDir.appendingPathComponent("icon_\(base)x\(base)@2x.png"))
}
// Loose PNGs for other bundlers.
write(png(1024), outDir.appendingPathComponent("icon-1024.png"))
for s in [32, 128, 256] { write(png(s), outDir.appendingPathComponent("\(s)x\(s).png")) }

// Windows ICO: PNG-compressed entries (supported since Vista).
let icoSizes = [16, 24, 32, 48, 64, 128, 256]
var ico = Data()
func le16(_ v: Int) { var x = UInt16(v).littleEndian; ico.append(Data(bytes: &x, count: 2)) }
func le32(_ v: Int) { var x = UInt32(v).littleEndian; ico.append(Data(bytes: &x, count: 4)) }
le16(0); le16(1); le16(icoSizes.count)
var offset = 6 + 16 * icoSizes.count
let blobs = icoSizes.map { png($0) }
for (s, b) in zip(icoSizes, blobs) {
    ico.append(UInt8(s >= 256 ? 0 : s)); ico.append(UInt8(s >= 256 ? 0 : s))
    ico.append(0); ico.append(0)       // palette, reserved
    le16(1); le16(32)                  // planes, bpp
    le32(b.count); le32(offset)
    offset += b.count
}
for b in blobs { ico.append(b) }
write(ico, outDir.appendingPathComponent("icon.ico"))

// ICNS via iconutil.
let task = Process()
task.executableURL = URL(fileURLWithPath: "/usr/bin/iconutil")
task.arguments = ["-c", "icns", iconsetDir.path, "-o", outDir.appendingPathComponent("Duet.icns").path]
try! task.run(); task.waitUntilExit()
print(task.terminationStatus == 0 ? "Wrote icons to \(outDir.path)" : "iconutil failed")
