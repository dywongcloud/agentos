import Foundation
import CoreMedia
import VideoToolbox

struct Codec {
    let name: String
    let type: CMVideoCodecType
}

let codecs = [
    Codec(name: "H.264", type: kCMVideoCodecType_H264),
    Codec(name: "HEVC", type: kCMVideoCodecType_HEVC),
    Codec(name: "AV1", type: kCMVideoCodecType_AV1),
]

func fourCC(_ value: UInt32) -> String {
    let bytes = [
        UInt8((value >> 24) & 0xff),
        UInt8((value >> 16) & 0xff),
        UInt8((value >> 8) & 0xff),
        UInt8(value & 0xff),
    ]
    return String(bytes: bytes, encoding: .ascii) ?? String(format: "0x%08x", value)
}

func dictionaryValue(_ dictionary: NSDictionary, _ key: CFString) -> Any? {
    dictionary[key as String]
}

func boolDescription(_ value: Any?) -> String {
    guard let number = value as? NSNumber else { return "missing" }
    return number.boolValue ? "true" : "false"
}

func encoderList() {
    var array: CFArray?
    let status = VTCopyVideoEncoderList(nil, &array)
    print("encoder_list status=\(status) count=\(array.map { CFArrayGetCount($0) } ?? 0)")
    guard status == noErr, let entries = array as? [NSDictionary] else { return }
    for codec in codecs {
        let matching = entries.filter {
            (dictionaryValue($0, kVTVideoEncoderList_CodecType) as? NSNumber)?.uint32Value == codec.type
        }
        print("encoder_list codec=\(codec.name) fourcc=\(fourCC(codec.type)) entries=\(matching.count)")
        for entry in matching {
            let identifier = dictionaryValue(entry, kVTVideoEncoderList_EncoderID) as? String ?? "missing"
            let name = dictionaryValue(entry, kVTVideoEncoderList_DisplayName) as? String ?? "missing"
            let hardware = boolDescription(dictionaryValue(entry, kVTVideoEncoderList_IsHardwareAccelerated))
            print("  encoder id=\(identifier) name=\(name) hardware=\(hardware)")
        }
    }
}

func hardwareSession(for codec: Codec) {
    let specification = [
        kVTVideoEncoderSpecification_RequireHardwareAcceleratedVideoEncoder as String: true
    ] as CFDictionary
    var session: VTCompressionSession?
    let status = VTCompressionSessionCreate(
        allocator: kCFAllocatorDefault,
        width: 1280,
        height: 720,
        codecType: codec.type,
        encoderSpecification: specification,
        imageBufferAttributes: nil,
        compressedDataAllocator: nil,
        outputCallback: nil,
        refcon: nil,
        compressionSessionOut: &session
    )
    guard status == noErr, let session else {
        print("hardware_encode codec=\(codec.name) create_status=\(status) created=false")
        return
    }
    defer { VTCompressionSessionInvalidate(session) }
    let hardwarePointer = UnsafeMutablePointer<CFTypeRef?>.allocate(capacity: 1)
    hardwarePointer.initialize(to: nil)
    defer {
        hardwarePointer.deinitialize(count: 1)
        hardwarePointer.deallocate()
    }
    let hardwareStatus = VTSessionCopyProperty(
        session,
        key: kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder,
        allocator: kCFAllocatorDefault,
        valueOut: hardwarePointer
    )
    let hardwareValue = hardwarePointer.pointee
    print("hardware_encode codec=\(codec.name) create_status=\(status) created=true hardware_query_status=\(hardwareStatus) hardware=\(boolDescription(hardwareValue))")

    var properties: CFDictionary?
    let propertyStatus = VTSessionCopySupportedPropertyDictionary(session, supportedPropertyDictionaryOut: &properties)
    let propertyDictionary: NSDictionary? = properties
    let keys = (propertyDictionary?.allKeys as? [String] ?? []).sorted()
    let screenTerms = ["screen", "palette", "intrablock", "intra_block", "intra block", "ibc", "scc"]
    let matches = keys.filter { key in
        let lower = key.lowercased()
        return screenTerms.contains { lower.contains($0) }
    }
    print("supported_properties codec=\(codec.name) status=\(propertyStatus) total=\(keys.count) screen_content_matches=\(matches)")

    guard codec.type == kCMVideoCodecType_HEVC else { return }
    let candidates = [
        "EnableHEVCScreenContentCoding",
        "HEVCScreenContentCoding",
        "EnablePaletteMode",
        "PaletteMode",
        "EnableIntraBlockCopy",
        "IntraBlockCopy",
        "EnableIBC",
        "HEVCSCC",
    ]
    for candidate in candidates {
        let candidateStatus = VTSessionSetProperty(
            session,
            key: candidate as CFString,
            value: kCFBooleanTrue
        )
        print("hevc_scc_property key=\(candidate) advertised=\(keys.contains(candidate)) set_true_status=\(candidateStatus)")
    }
}

print("probe=VideoToolboxCapabilityProbe")
print("os=\(ProcessInfo.processInfo.operatingSystemVersionString) arch=\(ProcessInfo.processInfo.environment["NATIVE_ARCH_ACTUAL"] ?? "runtime-arm64")")
encoderList()
for codec in codecs {
    print("hardware_decode codec=\(codec.name) supported=\(VTIsHardwareDecodeSupported(codec.type))")
    hardwareSession(for: codec)
}
