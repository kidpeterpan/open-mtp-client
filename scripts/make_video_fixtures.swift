#!/usr/bin/env swift
// Makes the small test videos used by internal/service/thumbnail (not shipped).
//
//   swift scripts/make_video_fixtures.swift internal/tests/fixtures/video
//
// Every clip is 2 s at 30 fps, H.264. Frame 0, AS SEEN, has a red top-left
// quadrant on a blue background, so a test can tell a right-way-up picture from
// a turned one by sampling the quadrants' centres.
//   moov_at_end.mp4  160x96, the index (moov) after the data, as a phone's camera writes it
//   faststart.mp4    160x96, the index before the data
//   portrait.mp4     stored 160x96 with a 90 degree track matrix: seen as 96x160
import AVFoundation
import AppKit

let outDir = URL(fileURLWithPath: CommandLine.arguments[1], isDirectory: true)

func make(_ name: String, faststart: Bool, rotate: Bool) throws {
    let url = outDir.appendingPathComponent(name)
    try? FileManager.default.removeItem(at: url)
    let w = 160, h = 96
    let writer = try AVAssetWriter(outputURL: url, fileType: .mp4)
    writer.shouldOptimizeForNetworkUse = faststart
    let input = AVAssetWriterInput(mediaType: .video, outputSettings: [
        AVVideoCodecKey: AVVideoCodecType.h264, AVVideoWidthKey: w, AVVideoHeightKey: h,
        AVVideoCompressionPropertiesKey: [AVVideoAverageBitRateKey: 200_000, AVVideoMaxKeyFrameIntervalKey: 30],
    ])
    if rotate { input.transform = CGAffineTransform(rotationAngle: .pi / 2) }
    let adaptor = AVAssetWriterInputPixelBufferAdaptor(assetWriterInput: input, sourcePixelBufferAttributes: [
        kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32ARGB,
        kCVPixelBufferWidthKey as String: w, kCVPixelBufferHeightKey as String: h,
    ])
    writer.add(input)
    writer.startWriting()
    writer.startSession(atSourceTime: .zero)
    for n in 0..<60 {
        while !input.isReadyForMoreMediaData { Thread.sleep(forTimeInterval: 0.002) }
        var buffer: CVPixelBuffer?
        CVPixelBufferPoolCreatePixelBuffer(nil, adaptor.pixelBufferPool!, &buffer)
        CVPixelBufferLockBaseAddress(buffer!, [])
        let ctx = CGContext(
            data: CVPixelBufferGetBaseAddress(buffer!), width: w, height: h, bitsPerComponent: 8,
            bytesPerRow: CVPixelBufferGetBytesPerRow(buffer!), space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.noneSkipFirst.rawValue)!
        ctx.setFillColor(CGColor(red: 0, green: 0, blue: 1, alpha: 1))
        ctx.fill(CGRect(x: 0, y: 0, width: w, height: h))
        // Core Graphics is y-up. A plain clip is stored as it is seen: red top-left
        // is (0, h/2). A clip turned 90 degrees by its track matrix lands the stored
        // bottom-left, (0, 0), at the seen top-left.
        ctx.setFillColor(CGColor(red: 1, green: 0, blue: 0, alpha: 1))
        ctx.fill(CGRect(x: 0, y: rotate ? 0 : h / 2, width: w / 2, height: h / 2))
        // Something moves, so later frames differ from frame 0.
        ctx.setFillColor(CGColor(red: 1, green: 1, blue: 1, alpha: 1))
        ctx.fill(CGRect(x: 90 + n % 60, y: 8, width: 6, height: 6))
        CVPixelBufferUnlockBaseAddress(buffer!, [])
        adaptor.append(buffer!, withPresentationTime: CMTime(value: CMTimeValue(n), timescale: 30))
    }
    input.markAsFinished()
    let done = DispatchSemaphore(value: 0)
    writer.finishWriting { done.signal() }
    done.wait()
    guard writer.status == .completed else { fatalError("\(name): \(String(describing: writer.error))") }
    print(name, (try FileManager.default.attributesOfItem(atPath: url.path)[.size] as! NSNumber).intValue, "bytes")
}

try make("moov_at_end.mp4", faststart: false, rotate: false)
try make("faststart.mp4", faststart: true, rotate: false)
try make("portrait.mp4", faststart: false, rotate: true)
