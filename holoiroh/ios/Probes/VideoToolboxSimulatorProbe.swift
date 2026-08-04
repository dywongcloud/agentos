import UIKit
import AVFoundation
import CoreMedia
import CoreVideo
import VideoToolbox

final class SampleCollector {
    private let lock = NSLock()
    private(set) var sample: CMSampleBuffer?
    private(set) var status: OSStatus = noErr

    func receive(status: OSStatus, sample: CMSampleBuffer?) {
        lock.lock()
        self.status = status
        if status == noErr, let sample, CMSampleBufferDataIsReady(sample) {
            self.sample = sample
        }
        lock.unlock()
    }
}

let compressionCallback: VTCompressionOutputCallback = { refcon, _, status, _, sample in
    guard let refcon else { return }
    let collector = Unmanaged<SampleCollector>.fromOpaque(refcon).takeUnretainedValue()
    collector.receive(status: status, sample: sample)
}

func makePixelBuffer(width: Int, height: Int) -> CVPixelBuffer? {
    var output: CVPixelBuffer?
    let attributes = [
        kCVPixelBufferIOSurfacePropertiesKey as String: [:] as [String: Any],
    ] as CFDictionary
    let status = CVPixelBufferCreate(
        kCFAllocatorDefault,
        width,
        height,
        kCVPixelFormatType_32BGRA,
        attributes,
        &output
    )
    guard status == kCVReturnSuccess, let output else { return nil }
    CVPixelBufferLockBaseAddress(output, [])
    defer { CVPixelBufferUnlockBaseAddress(output, []) }
    guard let base = CVPixelBufferGetBaseAddress(output) else { return nil }
    let rowBytes = CVPixelBufferGetBytesPerRow(output)
    let pointer = base.assumingMemoryBound(to: UInt8.self)
    for y in 0..<height {
        let row = pointer.advanced(by: y * rowBytes)
        for x in 0..<width {
            let offset = x * 4
            row[offset] = UInt8((x * 3) & 0xff)
            row[offset + 1] = UInt8((y * 5) & 0xff)
            row[offset + 2] = (x / 8 + y / 8).isMultiple(of: 2) ? 240 : 24
            row[offset + 3] = 255
        }
    }
    return output
}

func makeCompressedSample(codec: CMVideoCodecType, profile: CFString) -> (OSStatus, CMSampleBuffer?) {
    guard let pixelBuffer = makePixelBuffer(width: 160, height: 96) else { return (-1, nil) }
    let collector = SampleCollector()
    var session: VTCompressionSession?
    let createStatus = VTCompressionSessionCreate(
        allocator: kCFAllocatorDefault,
        width: 160,
        height: 96,
        codecType: codec,
        encoderSpecification: nil,
        imageBufferAttributes: nil,
        compressedDataAllocator: nil,
        outputCallback: compressionCallback,
        refcon: Unmanaged.passUnretained(collector).toOpaque(),
        compressionSessionOut: &session
    )
    guard createStatus == noErr, let session else { return (createStatus, nil) }
    defer { VTCompressionSessionInvalidate(session) }
    for (key, value) in [
        (kVTCompressionPropertyKey_RealTime, kCFBooleanTrue as CFTypeRef),
        (kVTCompressionPropertyKey_AllowFrameReordering, kCFBooleanFalse as CFTypeRef),
        (kVTCompressionPropertyKey_ProfileLevel, profile as CFTypeRef),
    ] {
        let status = VTSessionSetProperty(session, key: key, value: value)
        guard status == noErr else { return (status, nil) }
    }
    let prepareStatus = VTCompressionSessionPrepareToEncodeFrames(session)
    guard prepareStatus == noErr else { return (prepareStatus, nil) }
    var info = VTEncodeInfoFlags()
    let encodeStatus = VTCompressionSessionEncodeFrame(
        session,
        imageBuffer: pixelBuffer,
        presentationTimeStamp: .zero,
        duration: CMTime(value: 1, timescale: 30),
        frameProperties: [kVTEncodeFrameOptionKey_ForceKeyFrame as String: true] as CFDictionary,
        sourceFrameRefcon: nil,
        infoFlagsOut: &info
    )
    guard encodeStatus == noErr else { return (encodeStatus, nil) }
    let completeStatus = VTCompressionSessionCompleteFrames(session, untilPresentationTimeStamp: .invalid)
    guard completeStatus == noErr else { return (completeStatus, nil) }
    return (collector.status, collector.sample)
}

func makeUncompressedSample() -> CMSampleBuffer? {
    guard let pixelBuffer = makePixelBuffer(width: 160, height: 96) else { return nil }
    var format: CMVideoFormatDescription?
    guard CMVideoFormatDescriptionCreateForImageBuffer(
        allocator: kCFAllocatorDefault,
        imageBuffer: pixelBuffer,
        formatDescriptionOut: &format
    ) == noErr, let format else { return nil }
    var timing = CMSampleTimingInfo(
        duration: .invalid,
        presentationTimeStamp: .zero,
        decodeTimeStamp: .invalid
    )
    var sample: CMSampleBuffer?
    guard CMSampleBufferCreateReadyWithImageBuffer(
        allocator: kCFAllocatorDefault,
        imageBuffer: pixelBuffer,
        formatDescription: format,
        sampleTiming: &timing,
        sampleBufferOut: &sample
    ) == noErr else { return nil }
    return sample
}

func tagForImmediateDisplay(_ sample: CMSampleBuffer) {
    guard let attachments = CMSampleBufferGetSampleAttachmentsArray(
        sample,
        createIfNecessary: true
    ) as? [CFMutableDictionary], let first = attachments.first else { return }
    CFDictionarySetValue(
        first,
        Unmanaged.passUnretained(kCMSampleAttachmentKey_DisplayImmediately).toOpaque(),
        Unmanaged.passUnretained(kCFBooleanTrue).toOpaque()
    )
}

func statusName(_ status: AVQueuedSampleBufferRenderingStatus) -> String {
    switch status {
    case .unknown: return "unknown"
    case .rendering: return "rendering"
    case .failed: return "failed"
    @unknown default: return "other-\(status.rawValue)"
    }
}

func loadAV1Sample() -> (String, CMSampleBuffer?) {
    guard let url = Bundle.main.url(forResource: "av1-probe", withExtension: "mp4") else {
        return ("resource_missing", nil)
    }
    let asset = AVURLAsset(url: url)
    guard let track = asset.tracks(withMediaType: .video).first else {
        return ("track_missing", nil)
    }
    do {
        let reader = try AVAssetReader(asset: asset)
        let output = AVAssetReaderTrackOutput(track: track, outputSettings: nil)
        guard reader.canAdd(output) else { return ("reader_output_rejected", nil) }
        reader.add(output)
        guard reader.startReading() else {
            return ("reader_start_\(reader.error?.localizedDescription ?? "failed")", nil)
        }
        guard let sample = output.copyNextSampleBuffer() else {
            return ("sample_missing_\(reader.error?.localizedDescription ?? "nil")", nil)
        }
        return ("0", sample)
    } catch {
        return (error.localizedDescription, nil)
    }
}

@main
final class ProbeApp: UIResponder, UIApplicationDelegate {
    var window: UIWindow?
    var layers = [(String, AVSampleBufferDisplayLayer)]()

    func application(
        _ application: UIApplication,
        didFinishLaunchingWithOptions launchOptions: [UIApplication.LaunchOptionsKey: Any]? = nil
    ) -> Bool {
        let window = UIWindow(frame: UIScreen.main.bounds)
        let controller = UIViewController()
        controller.view.backgroundColor = .black
        window.rootViewController = controller
        window.makeKeyAndVisible()
        self.window = window

        print("probe=VideoToolboxSimulatorProbe model=\(UIDevice.current.model) system=\(UIDevice.current.systemName)-\(UIDevice.current.systemVersion) arch=arm64")
        let codecs = [
            ("H.264", kCMVideoCodecType_H264),
            ("HEVC", kCMVideoCodecType_HEVC),
            ("AV1", kCMVideoCodecType_AV1),
        ]
        for (name, type) in codecs {
            print("hardware_decode codec=\(name) supported=\(VTIsHardwareDecodeSupported(type))")
        }

        let compressed = [
            ("H.264-compressed", makeCompressedSample(codec: kCMVideoCodecType_H264, profile: kVTProfileLevel_H264_Baseline_AutoLevel)),
            ("HEVC-compressed", makeCompressedSample(codec: kCMVideoCodecType_HEVC, profile: kVTProfileLevel_HEVC_Main_AutoLevel)),
        ]
        var row = 0
        for (name, result) in compressed {
            print("sample_create name=\(name) status=\(result.0) sample=\(result.1 != nil)")
            if let sample = result.1 {
                tagForImmediateDisplay(sample)
                enqueue(name: name, sample: sample, row: row, in: controller.view)
                row += 1
            }
        }
        if let sample = makeUncompressedSample() {
            tagForImmediateDisplay(sample)
            print("sample_create name=BGRA-uncompressed status=0 sample=true")
            enqueue(name: "BGRA-uncompressed", sample: sample, row: row, in: controller.view)
        } else {
            print("sample_create name=BGRA-uncompressed status=-1 sample=false")
        }
        let av1 = loadAV1Sample()
        print("sample_create name=AV1-compressed status=\(av1.0) sample=\(av1.1 != nil)")
        if let sample = av1.1 {
            tagForImmediateDisplay(sample)
            enqueue(name: "AV1-compressed", sample: sample, row: row + 1, in: controller.view)
        }

        DispatchQueue.main.asyncAfter(deadline: .now() + 5.0) {
            for (name, layer) in self.layers {
                let renderer = layer.sampleBufferRenderer
                print("display_layer name=\(name) status=\(statusName(renderer.status)) ready_for_more=\(renderer.isReadyForMoreMediaData) requires_flush=\(renderer.requiresFlushToResumeDecoding) error=\(renderer.error?.localizedDescription ?? "nil")")
            }
            fflush(stdout)
            exit(0)
        }
        return true
    }

    func enqueue(name: String, sample: CMSampleBuffer, row: Int, in view: UIView) {
        let layer = AVSampleBufferDisplayLayer()
        layer.frame = CGRect(x: 0, y: CGFloat(row * 110), width: 320, height: 100)
        layer.videoGravity = .resizeAspect
        view.layer.addSublayer(layer)
        let renderer = layer.sampleBufferRenderer
        print("display_layer_config name=\(name) initial_status=\(statusName(renderer.status)) ready_for_more=\(renderer.isReadyForMoreMediaData)")
        renderer.enqueue(sample)
        layers.append((name, layer))
    }
}
