import AVFoundation
import CoreMedia
import Darwin
import Foundation
import ScreenCaptureKit

private struct CaptureFailure: Error, CustomStringConvertible {
    let description: String
    init(_ message: String) { description = message }
}

// Each append is independently recoverable after a crash. The Rust reader ignores
// an incomplete trailing float. There is no WAV header that needs a clean close.
private final class PCMWriter {
    private let descriptor: Int32
    private let lock = NSLock()
    private var converter: AVAudioConverter?
    private var lastSync = Date()
    private var closed = false
    private var writtenFrames: Int64 = 0
    private let outputFormat = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 16000, channels: 1, interleaved: false)!

    init(_ path: String) throws {
        descriptor = Darwin.open(path, O_WRONLY | O_CREAT | O_EXCL, 0o600)
        guard descriptor >= 0 else {
            throw CaptureFailure("Cannot create recording file")
        }
    }

    deinit { if !closed { _ = Darwin.fsync(descriptor); _ = Darwin.close(descriptor) } }

    private func writeBytes(_ bytes: UnsafeRawPointer, count: Int) throws {
        var offset = 0
        while offset < count {
            let written = Darwin.write(descriptor, bytes.advanced(by: offset), count - offset)
            if written < 0 && errno == EINTR { continue }
            guard written > 0 else { throw CaptureFailure("Recording write failed: \(String(cString: strerror(errno)))") }
            offset += written
        }
    }

    // Offset is measured against the same host-clock origin for every source.
    // Never infer chronology from callback arrival, which includes queue latency.
    func append(_ input: AVAudioPCMBuffer, atSeconds offset: Double) throws {
        lock.lock()
        defer { lock.unlock() }
        guard !closed else { return }
        guard offset.isFinite, abs(offset) < 24 * 60 * 60 else { throw CaptureFailure("Capture timestamp is outside the supported 24-hour meeting timeline") }
        let position = Int64((offset * 16000).rounded())
        if converter == nil || converter?.inputFormat != input.format {
            converter = AVAudioConverter(from: input.format, to: outputFormat)
            converter?.primeMethod = .none
        }
        guard let converter = converter else { throw CaptureFailure("Unsupported capture audio format") }
        let capacity = AVAudioFrameCount(ceil(Double(input.frameLength) * 16000 / input.format.sampleRate) + 64)
        guard let output = AVAudioPCMBuffer(pcmFormat: outputFormat, frameCapacity: capacity) else {
            throw CaptureFailure("Cannot allocate capture audio buffer")
        }
        var provided = false
        var conversionError: NSError?
        let status = converter.convert(to: output, error: &conversionError) { _, state in
            if provided { state.pointee = .noDataNow; return nil }
            provided = true
            state.pointee = .haveData
            return input
        }
        guard status != .error, conversionError == nil else {
            throw conversionError ?? CaptureFailure("Audio conversion failed") as NSError
        }
        if let samples = output.floatChannelData, output.frameLength > 0 {
            if position > writtenFrames {
                // Real zero samples preserve delayed starts and dropped buffers
                // through restart, even when the process never closes the file.
                let zeros = [Float](repeating: 0, count: 4096)
                try zeros.withUnsafeBytes { bytes in
                    while writtenFrames < position {
                        let frames = Int(min(4096, position - writtenFrames))
                        try writeBytes(bytes.baseAddress!, count: frames * 4)
                        writtenFrames += Int64(frames)
                    }
                }
            }
            let skip = min(Int64(output.frameLength), max(0, writtenFrames - position))
            let frames = Int64(output.frameLength) - skip
            try writeBytes(UnsafeRawPointer(samples[0].advanced(by: Int(skip))), count: Int(frames) * 4)
            writtenFrames += frames
        }
        if Date().timeIntervalSince(lastSync) >= 1 {
            guard Darwin.fsync(descriptor) == 0 else { throw CaptureFailure("Cannot flush recording audio to disk") }
            lastSync = Date()
        }
    }

    func close() throws {
        lock.lock()
        defer { lock.unlock() }
        guard !closed else { return }
        closed = true
        let synchronized = Darwin.fsync(descriptor)
        let result = Darwin.close(descriptor)
        guard synchronized == 0 && result == 0 else { throw CaptureFailure("Cannot finalize recording audio on disk") }
    }
}

private final class MeetingCapture: NSObject {
    let originHostSeconds = CMClockGetTime(CMClockGetHostTimeClock()).seconds
    let startedAtMilliseconds = Int64((Date().timeIntervalSince1970 * 1000).rounded())
    var engine: AVAudioEngine?
    var microphoneWriter: PCMWriter?
    var systemWriter: PCMWriter?
    var screen: AnyObject?
    private let errorLock = NSLock()
    private var failure: String?

    func fail(_ error: Error) {
        errorLock.lock()
        if failure == nil { failure = String(describing: error) }
        errorLock.unlock()
    }
    func error() -> String? {
        errorLock.lock()
        defer { errorLock.unlock() }
        return failure
    }

    func startMicrophone(_ directory: String) throws {
        switch AVCaptureDevice.authorizationStatus(for: .audio) {
        case .authorized: break
        case .notDetermined:
            let permission = DispatchSemaphore(value: 0)
            var granted = false
            AVCaptureDevice.requestAccess(for: .audio) { allowed in granted = allowed; permission.signal() }
            guard permission.wait(timeout: .now() + 60) == .success, granted else {
                throw CaptureFailure("Microphone permission was not granted. Enable Handy in System Settings > Privacy & Security > Microphone, then retry.")
            }
        default:
            throw CaptureFailure("Microphone access is denied. Enable Handy in System Settings > Privacy & Security > Microphone.")
        }
        let writer = try PCMWriter(directory + "/microphone.pcm")
        microphoneWriter = writer
        let audioEngine = AVAudioEngine()
        let input = audioEngine.inputNode
        let format = input.outputFormat(forBus: 0)
        guard format.sampleRate > 0, format.channelCount > 0 else { throw CaptureFailure("No microphone input device is available") }
        input.installTap(onBus: 0, bufferSize: 4096, format: format) { [weak self] buffer, time in
            guard let self = self else { return }
            guard time.isHostTimeValid else { self.fail(CaptureFailure("Microphone did not provide a host-clock timestamp")); return }
            let hostSeconds = AVAudioTime.seconds(forHostTime: time.hostTime)
            do { try writer.append(buffer, atSeconds: hostSeconds - self.originHostSeconds) } catch { self.fail(error) }
        }
        engine = audioEngine
        audioEngine.prepare()
        try audioEngine.start()
    }

    func stop() -> String? {
        if let engine = engine {
            engine.inputNode.removeTap(onBus: 0)
            engine.stop()
            self.engine = nil
        }
        if #available(macOS 13.0, *), let capture = screen as? SystemCapture {
            do { try capture.stop() } catch { fail(error) }
        }
        screen = nil
        do { try microphoneWriter?.close() } catch { fail(error) }
        do { try systemWriter?.close() } catch { fail(error) }
        return error()
    }
}

@available(macOS 13.0, *)
private final class SystemCapture: NSObject, SCStreamOutput, SCStreamDelegate {
    private var stream: SCStream?
    private let owner: MeetingCapture
    private let writer: PCMWriter
    private let queue = DispatchQueue(label: "handy.meeting.system-audio")
    private let lifecycleLock = NSLock()
    private var cancelled = false

    init(owner: MeetingCapture, writer: PCMWriter) { self.owner = owner; self.writer = writer }

    func start() throws {
        let ready = DispatchSemaphore(value: 0)
        var content: SCShareableContent?
        var failure: Error?
        SCShareableContent.getExcludingDesktopWindows(false, onScreenWindowsOnly: true) { result, error in
            content = result; failure = error; ready.signal()
        }
        guard ready.wait(timeout: .now() + 60) == .success else { throw CaptureFailure("Screen recording permission request timed out; no system audio was started") }
        if let failure = failure { throw CaptureFailure("System audio capture unavailable: \(failure.localizedDescription). Check Screen & System Audio Recording permission in System Settings.") }
        guard let display = content?.displays.first else { throw CaptureFailure("No display is available for system audio capture") }
        let configuration = SCStreamConfiguration()
        configuration.width = 2
        configuration.height = 2
        configuration.minimumFrameInterval = CMTime(value: 1, timescale: 1)
        configuration.capturesAudio = true
        configuration.excludesCurrentProcessAudio = true
        configuration.sampleRate = 16000
        configuration.channelCount = 1
        let capture = SCStream(filter: SCContentFilter(display: display, excludingWindows: []), configuration: configuration, delegate: self)
        try capture.addStreamOutput(self, type: .audio, sampleHandlerQueue: queue)
        stream = capture
        let started = DispatchSemaphore(value: 0)
        capture.startCapture { [weak self] error in
            failure = error
            self?.lifecycleLock.lock()
            let cancelled = self?.cancelled ?? true
            self?.lifecycleLock.unlock()
            if cancelled { capture.stopCapture(completionHandler: nil) }
            started.signal()
        }
        guard started.wait(timeout: .now() + 20) == .success else { throw CaptureFailure("System audio capture start timed out") }
        if let failure = failure { throw failure }
    }

    func stop() throws {
        lifecycleLock.lock()
        cancelled = true
        lifecycleLock.unlock()
        guard let stream = stream else { return }
        self.stream = nil
        let stopped = DispatchSemaphore(value: 0)
        var failure: Error?
        stream.stopCapture { error in failure = error; stopped.signal() }
        guard stopped.wait(timeout: .now() + 20) == .success else { throw CaptureFailure("System capture stop timed out") }
        queue.sync {}
        if let failure = failure { throw failure }
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) { owner.fail(error) }

    func stream(_ stream: SCStream, didOutputSampleBuffer sampleBuffer: CMSampleBuffer, of type: SCStreamOutputType) {
        guard type == .audio, sampleBuffer.isValid, let description = sampleBuffer.formatDescription,
              let basic = CMAudioFormatDescriptionGetStreamBasicDescription(description),
              let format = AVAudioFormat(streamDescription: basic) else { return }
        var required = 0
        CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(sampleBuffer, bufferListSizeNeededOut: &required, bufferListOut: nil, bufferListSize: 0, blockBufferAllocator: nil, blockBufferMemoryAllocator: nil, flags: 0, blockBufferOut: nil)
        let storage = UnsafeMutableRawPointer.allocate(byteCount: required, alignment: MemoryLayout<AudioBufferList>.alignment)
        defer { storage.deallocate() }
        let list = storage.bindMemory(to: AudioBufferList.self, capacity: 1)
        var block: CMBlockBuffer?
        let status = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(sampleBuffer, bufferListSizeNeededOut: nil, bufferListOut: list, bufferListSize: required, blockBufferAllocator: nil, blockBufferMemoryAllocator: nil, flags: 0, blockBufferOut: &block)
        guard status == noErr, let buffer = AVAudioPCMBuffer(pcmFormat: format, bufferListNoCopy: list, deallocator: nil) else {
            owner.fail(CaptureFailure("Cannot read captured system audio buffer")); return
        }
        buffer.frameLength = AVAudioFrameCount(CMSampleBufferGetNumSamples(sampleBuffer))
        guard let clock = stream.synchronizationClock else {
            owner.fail(CaptureFailure("System audio did not provide a synchronization clock")); return
        }
        let presentation = CMSampleBufferGetPresentationTimeStamp(sampleBuffer)
        let hostTime = CMSyncConvertTime(presentation, from: clock, to: CMClockGetHostTimeClock())
        guard hostTime.isValid, hostTime.isNumeric else {
            owner.fail(CaptureFailure("System audio did not provide a valid presentation timestamp")); return
        }
        do { try writer.append(buffer, atSeconds: hostTime.seconds - owner.originHostSeconds) } catch { owner.fail(error) }
        withExtendedLifetime(block) {}
    }
}

private var activeCapture: MeetingCapture?
private let sessionLock = NSLock()

@_cdecl("handy_meeting_system_available")
public func handyMeetingSystemAvailable() -> Bool {
    if #available(macOS 13.0, *) { return true }
    return false
}

@_cdecl("handy_meeting_start")
public func handyMeetingStart(_ directory: UnsafePointer<CChar>, _ microphone: Bool, _ systemAudio: Bool) -> UnsafeMutablePointer<CChar>? {
    sessionLock.lock()
    guard activeCapture == nil else { sessionLock.unlock(); return strdup("A meeting is already recording") }
    let capture = MeetingCapture()
    activeCapture = capture
    sessionLock.unlock()
    let path = String(cString: directory)
    do {
        let manifest: [String: Any] = ["version": 1, "alignment": "monotonic_pcm_v1", "synchronized": true, "sample_rate": 16000, "started_at_ms": capture.startedAtMilliseconds, "clock": "macos_host_clock", "origin_host_seconds": capture.originHostSeconds]
        let manifestPath = URL(fileURLWithPath: path).appendingPathComponent("capture-timeline.json")
        let manifestData = try JSONSerialization.data(withJSONObject: manifest, options: [.sortedKeys])
        try manifestData.write(to: manifestPath, options: [.atomic])
        try FileManager.default.setAttributes([.posixPermissions: 0o600], ofItemAtPath: manifestPath.path)
        let manifestDescriptor = Darwin.open(manifestPath.path, O_RDONLY)
        guard manifestDescriptor >= 0 else { throw CaptureFailure("Cannot open capture timeline manifest") }
        let manifestSynced = Darwin.fsync(manifestDescriptor)
        _ = Darwin.close(manifestDescriptor)
        let directoryDescriptor = Darwin.open(path, O_RDONLY)
        guard directoryDescriptor >= 0 else { throw CaptureFailure("Cannot open capture timeline directory") }
        let directorySynced = Darwin.fsync(directoryDescriptor)
        _ = Darwin.close(directoryDescriptor)
        guard manifestSynced == 0, directorySynced == 0 else { throw CaptureFailure("Cannot persist capture timeline manifest") }
        if microphone { try capture.startMicrophone(path) }
        if systemAudio {
            if #available(macOS 13.0, *) {
                let writer = try PCMWriter(path + "/system.pcm")
                capture.systemWriter = writer
                let system = SystemCapture(owner: capture, writer: writer)
                capture.screen = system
                try system.start()
            } else { throw CaptureFailure("System audio requires macOS 13 or later") }
        }
        return nil
    } catch {
        _ = capture.stop()
        sessionLock.lock()
        activeCapture = nil
        sessionLock.unlock()
        return strdup(String(describing: error))
    }
}

@_cdecl("handy_meeting_stop")
public func handyMeetingStop() -> UnsafeMutablePointer<CChar>? {
    sessionLock.lock()
    guard let capture = activeCapture else { sessionLock.unlock(); return strdup("No meeting is recording") }
    activeCapture = nil
    sessionLock.unlock()
    if let failure = capture.stop() { return strdup(failure) }
    return nil
}

@_cdecl("handy_meeting_error")
public func handyMeetingError() -> UnsafeMutablePointer<CChar>? {
    sessionLock.lock()
    let capture = activeCapture
    sessionLock.unlock()
    if let failure = capture?.error() { return strdup(failure) }
    return nil
}

#if MEETING_CAPTURE_TEST
@main
private struct SyntheticPCMWriterTest {
    static func main() throws {
        let directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(at: directory) }
        let path = directory.appendingPathComponent("synthetic.pcm").path
        let writer = try PCMWriter(path)
        let format = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 48000, channels: 2, interleaved: false)!
        let buffer = AVAudioPCMBuffer(pcmFormat: format, frameCapacity: 4800)!
        buffer.frameLength = 4800
        for frame in 0..<4800 {
            buffer.floatChannelData![0][frame] = 0.25
            buffer.floatChannelData![1][frame] = 0.25
        }
        for index in 0..<10 { try writer.append(buffer, atSeconds: Double(index) / 10) }
        try writer.close()
        let data = try Data(contentsOf: URL(fileURLWithPath: path))
        precondition(data.count > 15000 * 4 && data.count <= 16000 * 4, "Resampling must produce approximately one second of mono 16k audio")
        precondition(data.count % 4 == 0, "Every persisted sample must be a complete float")
        let maximum = data.withUnsafeBytes { bytes in bytes.bindMemory(to: Float.self).map { abs($0) }.max() ?? 0 }
        precondition(maximum > 0.1 && maximum < 1, "Synthetic speech samples must survive conversion")
        do {
            _ = try PCMWriter(path)
            preconditionFailure("Opening an existing recording must not truncate it")
        } catch {}
        let alignedPath = directory.appendingPathComponent("aligned.pcm").path
        let aligned = try PCMWriter(alignedPath)
        let mono = AVAudioFormat(commonFormat: .pcmFormatFloat32, sampleRate: 16000, channels: 1, interleaved: false)!
        let chunk = AVAudioPCMBuffer(pcmFormat: mono, frameCapacity: 1600)!
        chunk.frameLength = 1600
        for frame in 0..<1600 { chunk.floatChannelData![0][frame] = 0.25 }
        try aligned.append(chunk, atSeconds: 0.2)
        try aligned.append(chunk, atSeconds: 0.5)
        try aligned.append(chunk, atSeconds: 0.55)
        try aligned.append(chunk, atSeconds: 0.55) // Duplicate must not extend the track.
        do {
            try aligned.append(chunk, atSeconds: .nan)
            preconditionFailure("Invalid timestamps must never create fictional chronology")
        } catch {}
        try aligned.close()
        let alignedData = try Data(contentsOf: URL(fileURLWithPath: alignedPath))
        let frames = alignedData.withUnsafeBytes { Array($0.bindMemory(to: Float.self)) }
        precondition(frames.count == 10400, "Offsets, gaps and overlap must determine the stored timeline")
        precondition(frames[0..<3200].allSatisfy { $0 == 0 }, "Delayed source start must remain silence")
        precondition(frames[3200..<4800].allSatisfy { abs($0 - 0.25) < 0.001 }, "Source samples must retain their initial offset")
        precondition(frames[4800..<8000].allSatisfy { $0 == 0 }, "Missing buffers must leave a real gap")
        precondition(frames[8000..<10400].allSatisfy { abs($0 - 0.25) < 0.001 }, "Overlapping buffers must be trimmed without shifting later audio")
        let anotherPath = directory.appendingPathComponent("second-source.pcm").path
        let another = try PCMWriter(anotherPath)
        try another.append(chunk, atSeconds: 0.4)
        try another.close()
        let anotherData = try Data(contentsOf: URL(fileURLWithPath: anotherPath))
        let anotherFrames = anotherData.withUnsafeBytes { Array($0.bindMemory(to: Float.self)) }
        precondition(anotherFrames.count == 8000 && anotherFrames[0..<6400].allSatisfy { $0 == 0 }, "A second source must share the first source's sample origin")
        print("Synthetic PCM resampling, persistence, overwrite protection, shared offsets, missing buffers and overlap trimming passed")
    }
}
#endif
