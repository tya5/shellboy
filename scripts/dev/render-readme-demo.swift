// Rasterize the recorded terminal cells; only the outer caption is editorial.
import AppKit

struct Frame: Decodable {
    let caption: String
    let cells: [[[String]]]
}

let directory = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)
let frames = try JSONDecoder().decode([Frame].self, from: Data(contentsOf: directory.appendingPathComponent("frames.json")))
let font = NSFont.monospacedSystemFont(ofSize: 16, weight: .regular)
let captionFont = NSFont.monospacedSystemFont(ofSize: 17, weight: .semibold)
let cellWidth = ("M" as NSString).size(withAttributes: [.font: font]).width
let cellHeight: CGFloat = 19

func color(_ hex: String, fallback: Int) -> NSColor {
    let rgb = Int(hex, radix: 16) ?? fallback
    return NSColor(srgbRed: CGFloat((rgb >> 16) & 255) / 255,
                   green: CGFloat((rgb >> 8) & 255) / 255,
                   blue: CGFloat(rgb & 255) / 255, alpha: 1)
}

for (index, frame) in frames.enumerated() {
    let width = ceil(cellWidth * CGFloat(frame.cells[0].count)) + 32
    let height = cellHeight * CGFloat(frame.cells.count) + 64
    let raster = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(width), pixelsHigh: Int(height),
                                  bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
                                  bytesPerRow: 0, bitsPerPixel: 0)!
    NSGraphicsContext.saveGraphicsState()
    NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: raster)
    color("eef2f6", fallback: 0xffffff).setFill()
    NSRect(x: 0, y: 0, width: width, height: height).fill()
    color("243447", fallback: 0).setFill()
    NSRect(x: 0, y: height - 40, width: width, height: 40).fill()
    (frame.caption as NSString).draw(at: NSPoint(x: 16, y: height - 30), withAttributes: [
        .font: captionFont, .foregroundColor: NSColor.white,
    ])
    for (rowIndex, row) in frame.cells.enumerated() {
        let y = height - 51 - cellHeight * CGFloat(rowIndex + 1)
        for (column, cell) in row.enumerated() {
            color(cell[2], fallback: 0xffffff).setFill()
            NSRect(x: 16 + cellWidth * CGFloat(column), y: y, width: cellWidth, height: cellHeight).fill()
        }
        for (column, cell) in row.enumerated() where !cell[0].isEmpty && cell[0] != " " {
            (cell[0] as NSString).draw(at: NSPoint(x: 16 + cellWidth * CGFloat(column), y: y),
                                       withAttributes: [.font: font, .foregroundColor: color(cell[1], fallback: 0x172638)])
        }
    }
    NSGraphicsContext.restoreGraphicsState()
    try raster.representation(using: .png, properties: [:])!.write(to: directory.appendingPathComponent("\(index).png"))
}
