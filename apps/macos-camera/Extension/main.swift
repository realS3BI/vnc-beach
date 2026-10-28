// VNC Beach virtual camera (CoreMediaIO camera extension).
//
// The device has two streams: a source stream that video apps (FaceTime,
// Zoom, ...) read, and a sink stream that the VNC Beach server feeds with the
// webcam frames it receives from a viewer (beach-server/src/camera/mac.rs).
// While nothing arrives, the source shows a placeholder picture.
//
// The sink stream's custom property `bcdm` is "1" while an app reads the
// source stream; the server polls it and only then asks the viewer for video.

import CoreMediaIO
import CoreText
import Foundation
import os.log

let width: Int32 = 1280
let height: Int32 = 720
let frameRate: Int32 = 30
// Fixed so the server can find the device by its UID.
let deviceID = UUID(uuidString: "6F3B1D52-6C0E-4D8C-9C1A-5B7E2A4F9D10")!
let demandProperty = CMIOExtensionProperty(rawValue: "4cc_bcdm_glob_0000")
// Sink frames older than this count as "no video": the placeholder returns.
let staleNanos: UInt64 = 500_000_000

let log = OSLog(subsystem: "at.schlossers.vnc-beach.camera", category: "camera")

func hostNanos() -> UInt64 {
    UInt64(CMClockGetTime(CMClockGetHostTimeClock()).seconds * 1_000_000_000)
}

final class ProviderSource: NSObject, CMIOExtensionProviderSource {
    private(set) var provider: CMIOExtensionProvider!
    private var device: DeviceSource!

    init(clientQueue: DispatchQueue?) {
        super.init()
        provider = CMIOExtensionProvider(source: self, clientQueue: clientQueue)
        device = DeviceSource(localizedName: "VNC Beach")
        do {
            try provider.addDevice(device.device)
        } catch {
            fatalError("Failed to add device: \(error.localizedDescription)")
        }
    }

    func connect(to client: CMIOExtensionClient) throws {}
    func disconnect(from client: CMIOExtensionClient) {}

    var availableProperties: Set<CMIOExtensionProperty> { [.providerManufacturer] }

    func providerProperties(forProperties properties: Set<CMIOExtensionProperty>) throws
        -> CMIOExtensionProviderProperties
    {
        let props = CMIOExtensionProviderProperties(dictionary: [:])
        if properties.contains(.providerManufacturer) {
            props.manufacturer = "VNC Beach"
        }
        return props
    }

    func setProviderProperties(_ providerProperties: CMIOExtensionProviderProperties) throws {}
}

final class DeviceSource: NSObject, CMIOExtensionDeviceSource {
    private(set) var device: CMIOExtensionDevice!
    private var source: StreamSource!
    private var sink: StreamSource!
    private let queue = DispatchQueue(label: "at.schlossers.vnc-beach.camera.device")
    private var timer: DispatchSourceTimer?
    private var sourceRunning = false
    private var sinkClient: CMIOExtensionClient?
    private var lastSinkFrame: UInt64 = 0
    private var placeholder: CVPixelBuffer?
    private var format: CMFormatDescription!

    init(localizedName: String) {
        super.init()
        device = CMIOExtensionDevice(
            localizedName: localizedName, deviceID: deviceID, legacyDeviceID: nil, source: self)
        var desc: CMFormatDescription?
        CMVideoFormatDescriptionCreate(
            allocator: kCFAllocatorDefault, codecType: kCVPixelFormatType_32BGRA,
            width: width, height: height, extensions: nil, formatDescriptionOut: &desc)
        format = desc!
        let duration = CMTime(value: 1, timescale: frameRate)
        let streamFormat = CMIOExtensionStreamFormat(
            formatDescription: format, maxFrameDuration: duration, minFrameDuration: duration,
            validFrameDurations: nil)
        source = StreamSource(name: "VNC Beach", direction: .source, format: streamFormat, device: self)
        sink = StreamSource(name: "VNC Beach Sink", direction: .sink, format: streamFormat, device: self)
        do {
            try device.addStream(source.stream)
            try device.addStream(sink.stream)
        } catch {
            fatalError("Failed to add streams: \(error.localizedDescription)")
        }
        placeholder = makePlaceholder()
    }

    var availableProperties: Set<CMIOExtensionProperty> { [.deviceTransportType, .deviceModel] }

    func deviceProperties(forProperties properties: Set<CMIOExtensionProperty>) throws
        -> CMIOExtensionDeviceProperties
    {
        let props = CMIOExtensionDeviceProperties(dictionary: [:])
        if properties.contains(.deviceTransportType) {
            props.transportType = 0x7669_7274  // kIOAudioDeviceTransportTypeVirtual ('virt')
        }
        if properties.contains(.deviceModel) {
            props.model = "VNC Beach"
        }
        return props
    }

    func setDeviceProperties(_ deviceProperties: CMIOExtensionDeviceProperties) throws {}

    var demand: Bool { queue.sync { sourceRunning } }

    // MARK: source stream (read by video apps)

    func startSource() {
        queue.async { [self] in
            sourceRunning = true
            notifyDemand(true)
            let timer = DispatchSource.makeTimerSource(queue: queue)
            timer.schedule(deadline: .now(), repeating: 1.0 / Double(frameRate), leeway: .milliseconds(2))
            timer.setEventHandler { [weak self] in self?.tick() }
            timer.resume()
            self.timer = timer
            os_log("source started", log: log)
        }
    }

    func stopSource() {
        queue.async { [self] in
            sourceRunning = false
            timer?.cancel()
            timer = nil
            notifyDemand(false)
            os_log("source stopped", log: log)
        }
    }

    private func notifyDemand(_ on: Bool) {
        sink.stream.notifyPropertiesChanged([
            demandProperty: CMIOExtensionPropertyState<AnyObject>(value: (on ? "1" : "0") as NSString)
        ])
    }

    /// Sends the placeholder while no sink frames arrive.
    private func tick() {
        let now = hostNanos()
        guard now &- lastSinkFrame > staleNanos, let buffer = placeholder else { return }
        var timing = CMSampleTimingInfo(
            duration: CMTime(value: 1, timescale: frameRate),
            presentationTimeStamp: CMClockGetTime(CMClockGetHostTimeClock()),
            decodeTimeStamp: .invalid)
        var sample: CMSampleBuffer?
        let err = CMSampleBufferCreateForImageBuffer(
            allocator: kCFAllocatorDefault, imageBuffer: buffer, dataReady: true,
            makeDataReadyCallback: nil, refcon: nil, formatDescription: format,
            sampleTiming: &timing, sampleBufferOut: &sample)
        if err == noErr, let sample {
            source.stream.send(sample, discontinuity: [], hostTimeInNanoseconds: now)
        }
    }

    // MARK: sink stream (fed by the VNC Beach server)

    func startSink(client: CMIOExtensionClient) {
        queue.async { [self] in
            sinkClient = client
            os_log("sink started", log: log)
            consume()
        }
    }

    func stopSink() {
        queue.async { [self] in
            sinkClient = nil
            os_log("sink stopped", log: log)
        }
    }

    private func consume() {
        guard let client = sinkClient else { return }
        sink.stream.consumeSampleBuffer(from: client) { [weak self] sample, sequence, _, more, _ in
            guard let self else { return }
            self.queue.async {
                if let sample {
                    let now = hostNanos()
                    self.lastSinkFrame = now
                    if self.sourceRunning {
                        self.source.stream.send(sample, discontinuity: [], hostTimeInNanoseconds: now)
                    }
                    self.sink.stream.notifyScheduledOutputChanged(
                        CMIOExtensionScheduledOutput(sequenceNumber: sequence, hostTimeInNanoseconds: now))
                }
                if sample != nil && more {
                    self.consume()
                } else {
                    self.queue.asyncAfter(deadline: .now() + .milliseconds(5)) { self.consume() }
                }
            }
        }
    }

    // MARK: placeholder picture

    private func makePlaceholder() -> CVPixelBuffer? {
        var buffer: CVPixelBuffer?
        let attrs = [kCVPixelBufferIOSurfacePropertiesKey: [:]] as CFDictionary
        guard CVPixelBufferCreate(
            kCFAllocatorDefault, Int(width), Int(height), kCVPixelFormatType_32BGRA, attrs, &buffer)
            == kCVReturnSuccess, let buffer
        else { return nil }
        CVPixelBufferLockBaseAddress(buffer, [])
        defer { CVPixelBufferUnlockBaseAddress(buffer, []) }
        guard let ctx = CGContext(
            data: CVPixelBufferGetBaseAddress(buffer), width: Int(width), height: Int(height),
            bitsPerComponent: 8, bytesPerRow: CVPixelBufferGetBytesPerRow(buffer),
            space: CGColorSpaceCreateDeviceRGB(),
            bitmapInfo: CGImageAlphaInfo.premultipliedFirst.rawValue | CGBitmapInfo.byteOrder32Little.rawValue)
        else { return buffer }
        ctx.setFillColor(CGColor(red: 0.06, green: 0.09, blue: 0.13, alpha: 1))
        ctx.fill(CGRect(x: 0, y: 0, width: Int(width), height: Int(height)))
        drawCentered(ctx, "VNC Beach", size: 64, y: CGFloat(height) / 2 + 20, alpha: 1)
        drawCentered(ctx, "Keine Kamera vom Viewer", size: 32, y: CGFloat(height) / 2 - 50, alpha: 0.6)
        return buffer
    }

    private func drawCentered(_ ctx: CGContext, _ text: String, size: CGFloat, y: CGFloat, alpha: CGFloat) {
        let font = CTFontCreateUIFontForLanguage(.system, size, nil)!
        let attrs: [NSAttributedString.Key: Any] = [
            NSAttributedString.Key(kCTFontAttributeName as String): font,
            NSAttributedString.Key(kCTForegroundColorAttributeName as String):
                CGColor(red: 1, green: 1, blue: 1, alpha: alpha),
        ]
        let line = CTLineCreateWithAttributedString(NSAttributedString(string: text, attributes: attrs))
        let bounds = CTLineGetBoundsWithOptions(line, .useOpticalBounds)
        ctx.textPosition = CGPoint(x: (CGFloat(width) - bounds.width) / 2 - bounds.minX, y: y)
        CTLineDraw(line, ctx)
    }
}

final class StreamSource: NSObject, CMIOExtensionStreamSource {
    private(set) var stream: CMIOExtensionStream!
    private let format: CMIOExtensionStreamFormat
    private let direction: CMIOExtensionStream.Direction
    private unowned let device: DeviceSource
    private var client: CMIOExtensionClient?

    init(name: String, direction: CMIOExtensionStream.Direction, format: CMIOExtensionStreamFormat,
         device: DeviceSource)
    {
        self.format = format
        self.direction = direction
        self.device = device
        super.init()
        stream = CMIOExtensionStream(
            localizedName: name, streamID: UUID(), direction: direction, clockType: .hostTime, source: self)
    }

    var formats: [CMIOExtensionStreamFormat] { [format] }

    var availableProperties: Set<CMIOExtensionProperty> {
        var props: Set<CMIOExtensionProperty> = [.streamActiveFormatIndex, .streamFrameDuration]
        if direction == .sink {
            props.formUnion([
                .streamSinkBufferQueueSize, .streamSinkBuffersRequiredForStartup,
                .streamSinkBufferUnderrunCount, .streamSinkEndOfData, demandProperty,
            ])
        }
        return props
    }

    func streamProperties(forProperties properties: Set<CMIOExtensionProperty>) throws
        -> CMIOExtensionStreamProperties
    {
        let props = CMIOExtensionStreamProperties(dictionary: [:])
        if properties.contains(.streamActiveFormatIndex) {
            props.activeFormatIndex = 0
        }
        if properties.contains(.streamFrameDuration) {
            props.frameDuration = CMTime(value: 1, timescale: frameRate)
        }
        if properties.contains(.streamSinkBufferQueueSize) {
            props.sinkBufferQueueSize = 2
        }
        if properties.contains(.streamSinkBuffersRequiredForStartup) {
            props.sinkBuffersRequiredForStartup = 1
        }
        if properties.contains(demandProperty) {
            props.setPropertyState(
                CMIOExtensionPropertyState(value: (device.demand ? "1" : "0") as NSString),
                forProperty: demandProperty)
        }
        return props
    }

    func setStreamProperties(_ streamProperties: CMIOExtensionStreamProperties) throws {}

    func authorizedToStartStream(for client: CMIOExtensionClient) -> Bool {
        self.client = client
        return true
    }

    func startStream() throws {
        switch direction {
        case .sink:
            if let client { device.startSink(client: client) }
        default:
            device.startSource()
        }
    }

    func stopStream() throws {
        switch direction {
        case .sink: device.stopSink()
        default: device.stopSource()
        }
    }
}

let providerSource = ProviderSource(clientQueue: nil)
CMIOExtensionProvider.startService(provider: providerSource.provider)
CFRunLoopRun()
