// VideoToolbox capability probe: which codec x chroma x bit-depth streams does THIS Mac
// decode in hardware? Feeds real streams to a VTDecompressionSession with
// RequireHardwareAcceleratedVideoDecoder=true, then again allowing software, and reports.
// Also lists VT encoders and their supported H.264/HEVC profile levels.
//
// Build: swiftc -O -swift-version 5 probe.swift -o probe \
//          -framework VideoToolbox -framework CoreMedia -framework CoreVideo -framework CoreFoundation
// Run:   ./probe <streams-dir>

import Foundation
import VideoToolbox
import CoreMedia
import CoreVideo

// MARK: - small utils

func fourccString(_ v: UInt32) -> String {
    let b = [UInt8((v >> 24) & 0xff), UInt8((v >> 16) & 0xff), UInt8((v >> 8) & 0xff), UInt8(v & 0xff)]
    return String(bytes: b, encoding: .ascii) ?? String(format: "0x%08x", v)
}

func osErr(_ s: OSStatus) -> String {
    switch s {
    case noErr: return "ok"
    case kVTCouldNotFindVideoDecoderErr: return "kVTCouldNotFindVideoDecoderErr(-12906)"
    case kVTVideoDecoderBadDataErr: return "kVTVideoDecoderBadDataErr(-12909)"
    case kVTVideoDecoderUnsupportedDataFormatErr: return "kVTVideoDecoderUnsupportedDataFormatErr(-12910)"
    case kVTVideoDecoderMalfunctionErr: return "kVTVideoDecoderMalfunctionErr(-12911)"
    case kVTInvalidSessionErr: return "kVTInvalidSessionErr(-12903)"
    case kVTParameterErr: return "kVTParameterErr(-12902)"
    case kVTFormatDescriptionChangeNotSupportedErr: return "kVTFormatDescriptionChangeNotSupportedErr(-12916)"
    default: return "OSStatus(\(s))"
    }
}

// MARK: - Annex B (H.264 / HEVC)

func annexBUnits(_ d: [UInt8]) -> [[UInt8]] {
    var units: [[UInt8]] = []
    var i = 0
    var start = -1
    let n = d.count
    while i + 2 < n {
        if d[i] == 0 && d[i + 1] == 0 && d[i + 2] == 1 {
            if start >= 0 {
                var end = i
                if end > start && d[end - 1] == 0 { end -= 1 } // 4-byte start code
                units.append(Array(d[start..<end]))
            }
            i += 3
            start = i
        } else {
            i += 1
        }
    }
    if start >= 0 && start < n { units.append(Array(d[start..<n])) }
    return units
}

struct NalStream {
    var paramSets: [[UInt8]] // H.264: [SPS, PPS]; HEVC: [VPS, SPS, PPS]
    var accessUnits: [[[UInt8]]]
}

func parseH264(_ d: [UInt8]) -> NalStream {
    var sps: [UInt8]? = nil, pps: [UInt8]? = nil
    var aus: [[[UInt8]]] = []
    var cur: [[UInt8]] = []
    var curHasVcl = false
    for u in annexBUnits(d) {
        guard let h = u.first else { continue }
        let t = h & 0x1f
        if t == 7 { if sps == nil { sps = u } }
        if t == 8 { if pps == nil { pps = u } }
        let isVcl = t >= 1 && t <= 5
        if isVcl {
            let firstMb = u.count > 1 && (u[1] & 0x80) != 0 // first_mb_in_slice ue(v) == 0
            if curHasVcl && firstMb { aus.append(cur); cur = []; curHasVcl = false }
            cur.append(u); curHasVcl = true
        } else {
            if curHasVcl { aus.append(cur); cur = []; curHasVcl = false }
            if t != 9 { cur.append(u) } // drop AUD
        }
    }
    if !cur.isEmpty { aus.append(cur) }
    return NalStream(paramSets: [sps ?? [], pps ?? []], accessUnits: aus)
}

func parseHEVC(_ d: [UInt8]) -> NalStream {
    var vps: [UInt8]? = nil, sps: [UInt8]? = nil, pps: [UInt8]? = nil
    var aus: [[[UInt8]]] = []
    var cur: [[UInt8]] = []
    var curHasVcl = false
    for u in annexBUnits(d) {
        guard u.count > 2 else { continue }
        let t = (u[0] >> 1) & 0x3f
        if t == 32 { if vps == nil { vps = u } }
        if t == 33 { if sps == nil { sps = u } }
        if t == 34 { if pps == nil { pps = u } }
        let isVcl = t < 32
        if isVcl {
            let firstSlice = (u[2] & 0x80) != 0 // first_slice_segment_in_pic_flag
            if curHasVcl && firstSlice { aus.append(cur); cur = []; curHasVcl = false }
            cur.append(u); curHasVcl = true
        } else {
            if curHasVcl { aus.append(cur); cur = []; curHasVcl = false }
            if t != 35 { cur.append(u) } // drop AUD
        }
    }
    if !cur.isEmpty { aus.append(cur) }
    return NalStream(paramSets: [vps ?? [], sps ?? [], pps ?? []], accessUnits: aus)
}

func lengthPrefixed(_ au: [[UInt8]]) -> [UInt8] {
    var out: [UInt8] = []
    for u in au {
        let n = UInt32(u.count)
        out.append(UInt8((n >> 24) & 0xff)); out.append(UInt8((n >> 16) & 0xff))
        out.append(UInt8((n >> 8) & 0xff)); out.append(UInt8(n & 0xff))
        out.append(contentsOf: u)
    }
    return out
}

// MARK: - IVF (VP9 / AV1)

func ivfFrames(_ d: [UInt8]) -> (fourcc: String, w: Int, h: Int, frames: [[UInt8]]) {
    precondition(d.count >= 32 && d[0] == 0x44 && d[1] == 0x4b && d[2] == 0x49 && d[3] == 0x46, "not IVF")
    let hdrLen = Int(d[6]) | Int(d[7]) << 8
    let fourcc = String(bytes: d[8..<12], encoding: .ascii) ?? "????"
    let w = Int(d[12]) | Int(d[13]) << 8
    let h = Int(d[14]) | Int(d[15]) << 8
    var frames: [[UInt8]] = []
    var i = hdrLen
    while i + 12 <= d.count {
        let sz = Int(d[i]) | Int(d[i + 1]) << 8 | Int(d[i + 2]) << 16 | Int(d[i + 3]) << 24
        i += 12
        guard i + sz <= d.count else { break }
        frames.append(Array(d[i..<i + sz]))
        i += sz
    }
    return (fourcc, w, h, frames)
}

// AV1 OBU walk: returns (type, full obu bytes incl. header)
func av1Obus(_ d: [UInt8]) -> [(type: Int, bytes: [UInt8])] {
    var out: [(Int, [UInt8])] = []
    var i = 0
    while i < d.count {
        let start = i
        let h = d[i]; i += 1
        let type = Int((h >> 3) & 0xf)
        let ext = (h >> 2) & 1
        let hasSize = (h >> 1) & 1
        if ext == 1 { i += 1 }
        var size = 0
        if hasSize == 1 {
            var shift = 0
            while i < d.count {
                let b = d[i]; i += 1
                size |= Int(b & 0x7f) << shift
                shift += 7
                if b & 0x80 == 0 { break }
            }
        } else {
            size = d.count - i
        }
        let end = min(i + size, d.count)
        out.append((type, Array(d[start..<end])))
        i = end
    }
    return out
}

// MARK: - format descriptions

func fdFromParamSets(hevc: Bool, sets: [[UInt8]]) -> (CMFormatDescription?, OSStatus) {
    var fd: CMFormatDescription? = nil
    let ptrs = sets.map { s -> UnsafeMutablePointer<UInt8> in
        let p = UnsafeMutablePointer<UInt8>.allocate(capacity: max(s.count, 1))
        p.initialize(from: s, count: s.count)
        return p
    }
    defer { ptrs.forEach { $0.deallocate() } }
    let cptrs: [UnsafePointer<UInt8>] = ptrs.map { UnsafePointer($0) }
    let sizes: [Int] = sets.map { $0.count }
    let st: OSStatus = cptrs.withUnsafeBufferPointer { pp in
        sizes.withUnsafeBufferPointer { sp in
            if hevc {
                return CMVideoFormatDescriptionCreateFromHEVCParameterSets(
                    allocator: kCFAllocatorDefault, parameterSetCount: sets.count,
                    parameterSetPointers: pp.baseAddress!, parameterSetSizes: sp.baseAddress!,
                    nalUnitHeaderLength: 4, extensions: nil, formatDescriptionOut: &fd)
            } else {
                return CMVideoFormatDescriptionCreateFromH264ParameterSets(
                    allocator: kCFAllocatorDefault, parameterSetCount: sets.count,
                    parameterSetPointers: pp.baseAddress!, parameterSetSizes: sp.baseAddress!,
                    nalUnitHeaderLength: 4, formatDescriptionOut: &fd)
            }
        }
    }
    return (fd, st)
}

func fdWithAtom(codec: CMVideoCodecType, w: Int, h: Int, atomName: String, atom: [UInt8]) -> (CMFormatDescription?, OSStatus) {
    var fd: CMFormatDescription? = nil
    let atoms: [String: Any] = [atomName: Data(atom) as CFData]
    let ext: [String: Any] = [kCMFormatDescriptionExtension_SampleDescriptionExtensionAtoms as String: atoms as CFDictionary]
    let st = CMVideoFormatDescriptionCreate(allocator: kCFAllocatorDefault, codecType: codec,
                                            width: Int32(w), height: Int32(h),
                                            extensions: ext as CFDictionary, formatDescriptionOut: &fd)
    return (fd, st)
}

// vpcC v1: profile, level, (bitDepth<<4 | chromaSubsampling<<1 | fullRange), primaries, transfer, matrix, initDataSize=0
func vpcC(profile: UInt8, bitDepth: UInt8, chroma: UInt8) -> [UInt8] {
    return [1, 0, 0, 0, profile, 10, (bitDepth << 4) | (chroma << 1) | 0, 1, 1, 1, 0, 0]
}

// av1C: marker|version, profile<<5|level, tier<<7|hbd<<6|12bit<<5|mono<<4|subx<<3|suby<<2|csp, reserved, configOBUs
func av1C(profile: UInt8, highBitDepth: Bool, subX: Bool, subY: Bool, seqHeader: [UInt8]) -> [UInt8] {
    var b: [UInt8] = [0x81, (profile << 5) | 8,
                      (highBitDepth ? 0x40 : 0) | (subX ? 0x08 : 0) | (subY ? 0x04 : 0), 0]
    b.append(contentsOf: seqHeader)
    return b
}

// MARK: - decode attempt

struct DecodeResult {
    var createStatus: OSStatus
    var usingHardware: Bool?
    var framesIn: Int
    var framesOut: Int
    var firstError: OSStatus?
    var outPixFmt: UInt32?
    var msPerFrame: Double = 0
}

func tryDecode(fd: CMFormatDescription, samples: [[UInt8]], requireHW: Bool) -> DecodeResult {
    var session: VTDecompressionSession? = nil
    var spec: [String: Any] = [:]
    if requireHW {
        spec[kVTVideoDecoderSpecification_RequireHardwareAcceleratedVideoDecoder as String] = true
    } else {
        spec[kVTVideoDecoderSpecification_EnableHardwareAcceleratedVideoDecoder as String] = true
    }
    let st = VTDecompressionSessionCreate(allocator: kCFAllocatorDefault, formatDescription: fd,
                                          decoderSpecification: spec as CFDictionary,
                                          imageBufferAttributes: nil, outputCallback: nil,
                                          decompressionSessionOut: &session)
    var res = DecodeResult(createStatus: st, usingHardware: nil, framesIn: 0, framesOut: 0, firstError: nil, outPixFmt: nil, msPerFrame: 0)
    guard st == noErr, let s = session else { return res }
    defer { VTDecompressionSessionInvalidate(s) }

    var hw: CFTypeRef? = nil
    if VTSessionCopyProperty(s, key: kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder,
                             allocator: kCFAllocatorDefault, valueOut: &hw) == noErr, let b = hw as? Bool {
        res.usingHardware = b
    }

    let lock = NSLock()
    var outCount = 0
    var firstErr: OSStatus? = nil
    var pixFmt: UInt32? = nil

    let t0 = DispatchTime.now().uptimeNanoseconds
    for (i, sample) in samples.enumerated() {
        var bb: CMBlockBuffer? = nil
        let bytes = UnsafeMutablePointer<UInt8>.allocate(capacity: sample.count)
        bytes.initialize(from: sample, count: sample.count)
        let bst = CMBlockBufferCreateWithMemoryBlock(allocator: kCFAllocatorDefault, memoryBlock: bytes,
                                                     blockLength: sample.count, blockAllocator: kCFAllocatorNull,
                                                     customBlockSource: nil, offsetToData: 0, dataLength: sample.count,
                                                     flags: 0, blockBufferOut: &bb)
        guard bst == noErr, let blockBuf = bb else { bytes.deallocate(); continue }
        var sb: CMSampleBuffer? = nil
        var timing = CMSampleTimingInfo(duration: CMTime(value: 1, timescale: 30),
                                        presentationTimeStamp: CMTime(value: CMTimeValue(i), timescale: 30),
                                        decodeTimeStamp: .invalid)
        var size = sample.count
        let sst = CMSampleBufferCreateReady(allocator: kCFAllocatorDefault, dataBuffer: blockBuf,
                                            formatDescription: fd, sampleCount: 1,
                                            sampleTimingEntryCount: 1, sampleTimingArray: &timing,
                                            sampleSizeEntryCount: 1, sampleSizeArray: &size,
                                            sampleBufferOut: &sb)
        guard sst == noErr, let sampleBuf = sb else { bytes.deallocate(); continue }
        res.framesIn += 1
        let dst = VTDecompressionSessionDecodeFrame(s, sampleBuffer: sampleBuf, flags: [], infoFlagsOut: nil) {
            status, _, image, _, _ in
            lock.lock(); defer { lock.unlock() }
            if status == noErr, let img = image {
                outCount += 1
                if pixFmt == nil { pixFmt = CVPixelBufferGetPixelFormatType(img) }
            } else if firstErr == nil {
                firstErr = status
            }
        }
        if dst != noErr {
            lock.lock(); if firstErr == nil { firstErr = dst }; lock.unlock()
        }
        bytes.deallocate()
    }
    VTDecompressionSessionFinishDelayedFrames(s)
    VTDecompressionSessionWaitForAsynchronousFrames(s)
    let t1 = DispatchTime.now().uptimeNanoseconds
    lock.lock()
    res.framesOut = outCount; res.firstError = firstErr; res.outPixFmt = pixFmt
    res.msPerFrame = res.framesIn > 0 ? Double(t1 - t0) / 1e6 / Double(res.framesIn) : 0
    lock.unlock()
    return res
}

// MARK: - stream loading

struct Stream {
    let name: String
    let fd: CMFormatDescription
    let samples: [[UInt8]]
}

func load(path: String) -> (Stream?, String) {
    let name = (path as NSString).lastPathComponent
    guard let data = FileManager.default.contents(atPath: path) else { return (nil, "unreadable") }
    let d = [UInt8](data)
    let parts = name.split(separator: "_").map(String.init) // codec_chroma_bits.ext
    guard parts.count == 3 else { return (nil, "bad name") }
    let codec = parts[0]
    let chroma = parts[1]
    let bits = Int(parts[2].split(separator: ".")[0]) ?? 8
    switch codec {
    case "h264", "hevc":
        let ns = codec == "hevc" ? parseHEVC(d) : parseH264(d)
        if ns.paramSets.contains(where: { $0.isEmpty }) { return (nil, "missing parameter sets") }
        let (fd, st) = fdFromParamSets(hevc: codec == "hevc", sets: ns.paramSets)
        guard st == noErr, let f = fd else { return (nil, "format description: \(osErr(st))") }
        return (Stream(name: name, fd: f, samples: ns.accessUnits.map(lengthPrefixed)), "")
    case "vp9":
        let ivf = ivfFrames(d)
        let (profile, sub): (UInt8, UInt8) = chroma == "444" ? (bits > 8 ? 3 : 1, 3) : (bits > 8 ? 2 : 0, 1)
        let (fd, st) = fdWithAtom(codec: kCMVideoCodecType_VP9, w: ivf.w, h: ivf.h, atomName: "vpcC",
                                  atom: vpcC(profile: profile, bitDepth: UInt8(bits), chroma: sub))
        guard st == noErr, let f = fd else { return (nil, "format description: \(osErr(st))") }
        return (Stream(name: name, fd: f, samples: ivf.frames), "")
    case "av1":
        let ivf = ivfFrames(d)
        guard let first = ivf.frames.first else { return (nil, "no frames") }
        guard let seq = av1Obus(first).first(where: { $0.type == 1 })?.bytes else { return (nil, "no sequence header OBU") }
        let profile: UInt8 = chroma == "444" ? 1 : (chroma == "422" ? 2 : 0)
        let subX = chroma != "444", subY = chroma == "420"
        let atom = av1C(profile: profile, highBitDepth: bits > 8, subX: subX, subY: subY, seqHeader: seq)
        let (fd, st) = fdWithAtom(codec: kCMVideoCodecType_AV1, w: ivf.w, h: ivf.h, atomName: "av1C", atom: atom)
        guard st == noErr, let f = fd else { return (nil, "format description: \(osErr(st))") }
        // strip temporal delimiters (type 2) as ISOBMFF samples do
        let samples = ivf.frames.map { fr -> [UInt8] in
            av1Obus(fr).filter { $0.type != 2 }.flatMap { $0.bytes }
        }
        return (Stream(name: name, fd: f, samples: samples), "")
    default:
        return (nil, "unknown codec \(codec)")
    }
}

// MARK: - encoder listing

func listEncoders() {
    print("\n== VideoToolbox encoders on this box ==")
    var arr: CFArray? = nil
    guard VTCopyVideoEncoderList(nil, &arr) == noErr, let list = arr as? [[String: Any]] else {
        print("  VTCopyVideoEncoderList failed"); return
    }
    for e in list {
        let name = e[kVTVideoEncoderList_EncoderName as String] as? String ?? "?"
        let id = e[kVTVideoEncoderList_EncoderID as String] as? String ?? "?"
        let ct = (e[kVTVideoEncoderList_CodecType as String] as? NSNumber)?.uint32Value ?? 0
        let hw = e[kVTVideoEncoderList_IsHardwareAccelerated as String] as? Bool
        print(String(format: "  %-8@ hw=%@  %@  [%@]", fourccString(ct) as NSString,
                     hw.map { $0 ? "yes" : "no " } ?? "?  " as NSString, name as NSString, id as NSString))
        if ct == kCMVideoCodecType_H264 || ct == kCMVideoCodecType_HEVC {
            var dict: CFDictionary? = nil
            var idOut: CFString? = nil
            let spec: [String: Any] = [kVTVideoEncoderSpecification_EncoderID as String: id]
            let st = VTCopySupportedPropertyDictionaryForEncoder(width: 1920, height: 1080, codecType: ct,
                                                                   encoderSpecification: spec as CFDictionary,
                                                                   encoderIDOut: &idOut, supportedPropertiesOut: &dict)
            if st == noErr, let props = dict as? [String: Any],
               let pl = props[kVTCompressionPropertyKey_ProfileLevel as String] as? [String: Any],
               let vals = pl[kVTPropertySupportedValueListKey as String] as? [String] {
                print("           profile levels: " + vals.joined(separator: ", "))
            }
        }
    }
}

// MARK: - main

let args = CommandLine.arguments
guard args.count >= 2 else { print("usage: probe <streams-dir>"); exit(2) }
let dir = args[1]

print("== Host ==")
var size = 0
sysctlbyname("machdep.cpu.brand_string", nil, &size, nil, 0)
var buf = [CChar](repeating: 0, count: size)
sysctlbyname("machdep.cpu.brand_string", &buf, &size, nil, 0)
print("  cpu: " + String(cString: buf))
print("  macOS: " + ProcessInfo.processInfo.operatingSystemVersionString)

print("\n== VTIsHardwareDecodeSupported (per codec, says nothing about chroma) ==")
for (n, c) in [("H.264", kCMVideoCodecType_H264), ("HEVC", kCMVideoCodecType_HEVC),
               ("VP9", kCMVideoCodecType_VP9), ("AV1", kCMVideoCodecType_AV1)] {
    print("  \(n): \(VTIsHardwareDecodeSupported(c))")
}

print("\n== Decode probe (real streams in \(dir)) ==")
print(String(format: "  %-18@ %-9@ %-34@ %-9@ %@", "stream" as NSString, "hw-req" as NSString,
             "result (hw required)" as NSString, "sw-ok" as NSString, "result (hw allowed, not required)" as NSString))
let files = (try? FileManager.default.contentsOfDirectory(atPath: dir))?.sorted() ?? []
for f in files where f.hasSuffix(".h264") || f.hasSuffix(".h265") || f.hasSuffix(".ivf") {
    let (s, err) = load(path: dir + "/" + f)
    guard let stream = s else { print("  \(f): load failed: \(err)"); continue }
    let hw = tryDecode(fd: stream.fd, samples: stream.samples, requireHW: true)
    let sw = tryDecode(fd: stream.fd, samples: stream.samples, requireHW: false)
    func fmt(_ r: DecodeResult) -> (String, String) {
        if r.createStatus != noErr { return ("NO", "session: \(osErr(r.createStatus))") }
        let pf = r.outPixFmt.map { fourccString($0) } ?? "-"
        let ok = r.framesOut == r.framesIn && r.framesIn > 0
        var detail = "\(r.framesOut)/\(r.framesIn) frames, out=\(pf)"
        if let hwp = r.usingHardware { detail += hwp ? ", hw" : ", SW" }
        detail += String(format: ", %.1f ms/f", r.msPerFrame)
        if let e = r.firstError { detail += ", err=\(osErr(e))" }
        return (ok ? "YES" : (r.framesOut > 0 ? "partial" : "NO"), detail)
    }
    let (a, ad) = fmt(hw)
    let (b, bd) = fmt(sw)
    print(String(format: "  %-18@ %-9@ %-34@ %-9@ %@", f as NSString, a as NSString, ad as NSString, b as NSString, bd as NSString))
}

listEncoders()
