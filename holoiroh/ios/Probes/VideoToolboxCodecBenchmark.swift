import Foundation
import CoreMedia
import CoreVideo
import VideoToolbox

let width = 1280
let height = 720
let frameRate = 30
let frameCount = 90
let targetBitrate = 4_000_000

struct CodecRun {
    let name: String
    let type: CMVideoCodecType
    let profile: CFString
}

final class EncodeState {
    private let lock = NSLock()
    private var submitted = [CMTimeValue: UInt64]()
    private(set) var samples = [CMSampleBuffer]()
    private(set) var latencyMilliseconds = [Double]()
    private(set) var callbackErrors = [OSStatus]()

    func submit(_ pts: CMTime) {
        lock.lock()
        submitted[pts.value] = DispatchTime.now().uptimeNanoseconds
        lock.unlock()
    }

    func failSubmission(_ pts: CMTime) {
        lock.lock()
        submitted.removeValue(forKey: pts.value)
        lock.unlock()
    }

    func receive(status: OSStatus, sample: CMSampleBuffer?) {
        lock.lock()
        defer { lock.unlock() }
        guard status == noErr, let sample, CMSampleBufferDataIsReady(sample) else {
            callbackErrors.append(status)
            return
        }
        let pts = CMSampleBufferGetPresentationTimeStamp(sample)
        if let start = submitted.removeValue(forKey: pts.value) {
            let elapsed = DispatchTime.now().uptimeNanoseconds - start
            latencyMilliseconds.append(Double(elapsed) / 1_000_000.0)
        }
        samples.append(sample)
    }
}

final class DecodeState: @unchecked Sendable {
    private let lock = NSLock()
    private(set) var frames = 0
    private(set) var dimensionErrors = 0
    private(set) var callbackErrors = [OSStatus]()
    private var absoluteError: UInt64 = 0
    private var squaredError: Double = 0
    private var comparedComponents: UInt64 = 0

    func receive(status: OSStatus, image: CVImageBuffer?, pts: CMTime) {
        guard status == noErr, let pixelBuffer = image else {
            lock.lock()
            callbackErrors.append(status)
            lock.unlock()
            return
        }
        CVPixelBufferLockBaseAddress(pixelBuffer, .readOnly)
        defer { CVPixelBufferUnlockBaseAddress(pixelBuffer, .readOnly) }
        guard CVPixelBufferGetWidth(pixelBuffer) == width,
              CVPixelBufferGetHeight(pixelBuffer) == height,
              let base = CVPixelBufferGetBaseAddress(pixelBuffer) else {
            lock.lock()
            dimensionErrors += 1
            lock.unlock()
            return
        }
        let rowBytes = CVPixelBufferGetBytesPerRow(pixelBuffer)
        let frameIndex = Int(pts.value)
        var localAbsolute: UInt64 = 0
        var localSquared = 0.0
        let pointer = base.assumingMemoryBound(to: UInt8.self)
        for y in 0..<height {
            let row = pointer.advanced(by: y * rowBytes)
            for x in 0..<width {
                let expected = desktopPixel(x: x, y: y, frame: frameIndex)
                let offset = x * 4
                let decoded = (row[offset], row[offset + 1], row[offset + 2])
                let errors = [
                    abs(Int(decoded.0) - Int(expected.0)),
                    abs(Int(decoded.1) - Int(expected.1)),
                    abs(Int(decoded.2) - Int(expected.2)),
                ]
                for error in errors {
                    localAbsolute += UInt64(error)
                    localSquared += Double(error * error)
                }
            }
        }
        lock.lock()
        frames += 1
        absoluteError += localAbsolute
        squaredError += localSquared
        comparedComponents += UInt64(width * height * 3)
        lock.unlock()
    }

    func metrics() -> (mae: Double, psnr: Double) {
        lock.lock()
        defer { lock.unlock() }
        guard comparedComponents > 0 else { return (.infinity, 0) }
        let mae = Double(absoluteError) / Double(comparedComponents)
        let mse = squaredError / Double(comparedComponents)
        let psnr = mse == 0 ? .infinity : 10.0 * log10(255.0 * 255.0 / mse)
        return (mae, psnr)
    }
}

let encodeCallback: VTCompressionOutputCallback = { refcon, _, status, _, sample in
    guard let refcon else { return }
    let state = Unmanaged<EncodeState>.fromOpaque(refcon).takeUnretainedValue()
    state.receive(status: status, sample: sample)
}

let decodeCallback: VTDecompressionOutputCallback = { refcon, _, status, _, image, pts, _ in
    guard let refcon else { return }
    let state = Unmanaged<DecodeState>.fromOpaque(refcon).takeUnretainedValue()
    state.receive(status: status, image: image, pts: pts)
}

func desktopPixel(x: Int, y: Int, frame: Int) -> (UInt8, UInt8, UInt8, UInt8) {
    var pixel: (UInt8, UInt8, UInt8, UInt8) = (34, 29, 25, 255)
    if y < 54 {
        pixel = (52, 48, 44, 255)
    } else if x < 220 {
        pixel = (45, 39, 35, 255)
    } else if x == 220 || y == 54 {
        pixel = (90, 84, 78, 255)
    }
    if x > 250 && x < 1240 && y > 80 && y < 690 {
        let scrolledY = (y + frame * 5) % 48
        if scrolledY < 2 {
            pixel = (86, 79, 72, 255)
        }
        let column = (x - 250) % 180
        if scrolledY >= 12 && scrolledY < 19 && column < 120 {
            let palette: [(UInt8, UInt8, UInt8, UInt8)] = [
                (214, 197, 133, 255),
                (130, 190, 226, 255),
                (151, 205, 160, 255),
                (208, 145, 180, 255),
            ]
            pixel = palette[((y + frame) / 48 + x / 180) % palette.count]
        }
    }
    for row in 0..<8 {
        let top = 90 + row * 54
        if y >= top && y < top + 30 && x >= 24 && x < 190 - (row % 3) * 18 {
            pixel = row == frame % 8 ? (125, 105, 82, 255) : (67, 59, 53, 255)
        }
    }
    let cursorX = 280 + (frame * 13) % 880
    let cursorY = 110 + (frame * 7) % 520
    if x >= cursorX && x < cursorX + 12 && y >= cursorY && y < cursorY + 18 {
        pixel = (246, 246, 246, 255)
    }
    return pixel
}

func makeFrame(_ frame: Int) -> CVPixelBuffer {
    var buffer: CVPixelBuffer?
    let attributes = [
        kCVPixelBufferIOSurfacePropertiesKey as String: [:] as [String: Any],
        kCVPixelBufferMetalCompatibilityKey as String: true,
    ] as CFDictionary
    let status = CVPixelBufferCreate(
        kCFAllocatorDefault,
        width,
        height,
        kCVPixelFormatType_32BGRA,
        attributes,
        &buffer
    )
    precondition(status == kCVReturnSuccess && buffer != nil)
    let pixelBuffer = buffer!
    CVPixelBufferLockBaseAddress(pixelBuffer, [])
    let rowBytes = CVPixelBufferGetBytesPerRow(pixelBuffer)
    let pointer = CVPixelBufferGetBaseAddress(pixelBuffer)!.assumingMemoryBound(to: UInt8.self)
    for y in 0..<height {
        let row = pointer.advanced(by: y * rowBytes)
        for x in 0..<width {
            let pixel = desktopPixel(x: x, y: y, frame: frame)
            let offset = x * 4
            row[offset] = pixel.0
            row[offset + 1] = pixel.1
            row[offset + 2] = pixel.2
            row[offset + 3] = pixel.3
        }
    }
    CVPixelBufferUnlockBaseAddress(pixelBuffer, [])
    return pixelBuffer
}

func setProperty(_ session: VTCompressionSession, key: CFString, value: CFTypeRef) throws {
    let status = VTSessionSetProperty(session, key: key, value: value)
    if status != noErr {
        throw NSError(domain: NSOSStatusErrorDomain, code: Int(status), userInfo: [NSLocalizedDescriptionKey: "VTSessionSetProperty failed for \(key): \(status)"])
    }
}

func percentile(_ values: [Double], _ fraction: Double) -> Double {
    guard !values.isEmpty else { return .infinity }
    let sorted = values.sorted()
    let index = min(sorted.count - 1, Int((Double(sorted.count - 1) * fraction).rounded()))
    return sorted[index]
}

func decode(samples: [CMSampleBuffer], codec: CodecRun) throws -> DecodeState {
    guard let first = samples.first,
          let format = CMSampleBufferGetFormatDescription(first) else {
        throw NSError(domain: "VideoToolboxCodecBenchmark", code: 1, userInfo: [NSLocalizedDescriptionKey: "No format description"])
    }
    let state = DecodeState()
    let specification = [
        kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder as String: true
    ] as CFDictionary
    let destination = [
        kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA,
        kCVPixelBufferWidthKey as String: width,
        kCVPixelBufferHeightKey as String: height,
        kCVPixelBufferIOSurfacePropertiesKey as String: [:] as [String: Any],
    ] as CFDictionary
    var session: VTDecompressionSession?
    let createStatus = VTDecompressionSessionCreate(
        allocator: kCFAllocatorDefault,
        formatDescription: format,
        decoderSpecification: specification,
        imageBufferAttributes: destination,
        outputCallback: nil,
        decompressionSessionOut: &session
    )
    guard createStatus == noErr, let session else {
        throw NSError(domain: NSOSStatusErrorDomain, code: Int(createStatus), userInfo: [NSLocalizedDescriptionKey: "Hardware decoder create failed for \(codec.name): \(createStatus)"])
    }
    defer { VTDecompressionSessionInvalidate(session) }
    for sample in samples {
        var info = VTDecodeInfoFlags()
        let status = VTDecompressionSessionDecodeFrame(
            session,
            sampleBuffer: sample,
            flags: VTDecodeFrameFlags(rawValue: 1 << 0),
            infoFlagsOut: &info,
            completionHandler: { status, _, image, _, pts, _ in
                state.receive(status: status, image: image, pts: pts)
            }
        )
        if status != noErr {
            throw NSError(domain: NSOSStatusErrorDomain, code: Int(status), userInfo: [NSLocalizedDescriptionKey: "Decode submit failed for \(codec.name): \(status)"])
        }
    }
    VTDecompressionSessionWaitForAsynchronousFrames(session)
    return state
}

func run(_ codec: CodecRun) throws {
    let encodeState = EncodeState()
    let specification = [
        kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder as String: true
    ] as CFDictionary
    var session: VTCompressionSession?
    let createStatus = VTCompressionSessionCreate(
        allocator: kCFAllocatorDefault,
        width: Int32(width),
        height: Int32(height),
        codecType: codec.type,
        encoderSpecification: specification,
        imageBufferAttributes: nil,
        compressedDataAllocator: nil,
        outputCallback: encodeCallback,
        refcon: Unmanaged.passUnretained(encodeState).toOpaque(),
        compressionSessionOut: &session
    )
    guard createStatus == noErr, let session else {
        print("benchmark codec=\(codec.name) create_status=\(createStatus) no_go=true")
        return
    }
    defer { VTCompressionSessionInvalidate(session) }
    try setProperty(session, key: kVTCompressionPropertyKey_RealTime, value: kCFBooleanTrue)
    try setProperty(session, key: kVTCompressionPropertyKey_AllowFrameReordering, value: kCFBooleanFalse)
    try setProperty(session, key: kVTCompressionPropertyKey_AverageBitRate, value: NSNumber(value: targetBitrate))
    try setProperty(session, key: kVTCompressionPropertyKey_ExpectedFrameRate, value: NSNumber(value: frameRate))
    try setProperty(session, key: kVTCompressionPropertyKey_MaxKeyFrameInterval, value: NSNumber(value: frameRate))
    try setProperty(session, key: kVTCompressionPropertyKey_ProfileLevel, value: codec.profile)
    let prepareStatus = VTCompressionSessionPrepareToEncodeFrames(session)
    guard prepareStatus == noErr else {
        throw NSError(domain: NSOSStatusErrorDomain, code: Int(prepareStatus), userInfo: [NSLocalizedDescriptionKey: "Prepare failed for \(codec.name): \(prepareStatus)"])
    }

    let sequenceStart = DispatchTime.now().uptimeNanoseconds
    for frameIndex in 0..<frameCount {
        let frame = makeFrame(frameIndex)
        let pts = CMTime(value: CMTimeValue(frameIndex), timescale: CMTimeScale(frameRate))
        encodeState.submit(pts)
        var info = VTEncodeInfoFlags()
        let status = VTCompressionSessionEncodeFrame(
            session,
            imageBuffer: frame,
            presentationTimeStamp: pts,
            duration: CMTime(value: 1, timescale: CMTimeScale(frameRate)),
            frameProperties: nil,
            sourceFrameRefcon: nil,
            infoFlagsOut: &info
        )
        if status != noErr {
            encodeState.failSubmission(pts)
            throw NSError(domain: NSOSStatusErrorDomain, code: Int(status), userInfo: [NSLocalizedDescriptionKey: "Encode submit failed for \(codec.name): \(status)"])
        }
    }
    let completeStatus = VTCompressionSessionCompleteFrames(session, untilPresentationTimeStamp: .invalid)
    guard completeStatus == noErr else {
        throw NSError(domain: NSOSStatusErrorDomain, code: Int(completeStatus), userInfo: [NSLocalizedDescriptionKey: "Complete failed for \(codec.name): \(completeStatus)"])
    }
    let sequenceMilliseconds = Double(DispatchTime.now().uptimeNanoseconds - sequenceStart) / 1_000_000.0
    let bytes = encodeState.samples.reduce(0) { $0 + CMSampleBufferGetTotalSampleSize($1) }
    let durationSeconds = Double(frameCount) / Double(frameRate)
    let bitrate = Double(bytes * 8) / durationSeconds
    let decodeState = try decode(samples: encodeState.samples, codec: codec)
    let metrics = decodeState.metrics()
    print(String(format: "benchmark codec=%@ frames_in=%d frames_encoded=%d frames_decoded=%d bytes=%d bitrate_bps=%.0f sequence_ms=%.3f encode_p50_ms=%.3f encode_p95_ms=%.3f encode_max_ms=%.3f decode_mae=%.4f decode_psnr_db=%.3f encode_callback_errors=%d decode_callback_errors=%d dimension_errors=%d", codec.name, frameCount, encodeState.samples.count, decodeState.frames, bytes, bitrate, sequenceMilliseconds, percentile(encodeState.latencyMilliseconds, 0.50), percentile(encodeState.latencyMilliseconds, 0.95), encodeState.latencyMilliseconds.max() ?? .infinity, metrics.mae, metrics.psnr, encodeState.callbackErrors.count, decodeState.callbackErrors.count, decodeState.dimensionErrors))
}

print("probe=VideoToolboxCodecBenchmark sequence=deterministic-desktop-v1 dimensions=\(width)x\(height) frames=\(frameCount) fps=\(frameRate) target_bitrate_bps=\(targetBitrate)")
let runs = [
    CodecRun(name: "H.264-Baseline", type: kCMVideoCodecType_H264, profile: kVTProfileLevel_H264_Baseline_AutoLevel),
    CodecRun(name: "HEVC-Main", type: kCMVideoCodecType_HEVC, profile: kVTProfileLevel_HEVC_Main_AutoLevel),
]
for codec in runs {
    do {
        try run(codec)
    } catch {
        print("benchmark codec=\(codec.name) error=\(error)")
        exit(1)
    }
}
