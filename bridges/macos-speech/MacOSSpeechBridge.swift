import AVFoundation
import CoreMedia
import Foundation
import ScreenCaptureKit
import Speech

private let outputLock = NSLock()

private func emit(_ payload: [String: Any]) {
    guard JSONSerialization.isValidJSONObject(payload),
          let data = try? JSONSerialization.data(withJSONObject: payload),
          let line = String(data: data, encoding: .utf8) else {
        return
    }
    outputLock.lock()
    FileHandle.standardOutput.write(Data((line + "\n").utf8))
    outputLock.unlock()
}

private func argumentValues(named name: String) -> [String] {
    var values: [String] = []
    var index = 1
    while index < CommandLine.arguments.count {
        if CommandLine.arguments[index] == name, index + 1 < CommandLine.arguments.count {
            values.append(CommandLine.arguments[index + 1])
            index += 2
        } else {
            index += 1
        }
    }
    return values
}

private func fail(_ message: String) -> Never {
    emit(["type": "error", "message": message])
    exit(1)
}

private func authorizeMicrophone() -> Bool {
    switch AVCaptureDevice.authorizationStatus(for: .audio) {
    case .authorized:
        return true
    case .notDetermined:
        let authorization = DispatchSemaphore(value: 0)
        var granted = false
        AVCaptureDevice.requestAccess(for: .audio) { result in
            granted = result
            authorization.signal()
        }
        guard authorization.wait(timeout: .now() + 30) != .timedOut else {
            return false
        }
        return granted
    default:
        return false
    }
}

private enum AudioSource: String {
    case microphone
    case systemAudio = "system_audio"
}

private final class RecognitionController {
    private let recognizer: SFSpeechRecognizer
    private let contextualTerms: [String]
    private let controlQueue = DispatchQueue(label: "realtime-translation.speech-control")
    private let requestLock = NSLock()
    private var request: SFSpeechAudioBufferRecognitionRequest?
    private var task: SFSpeechRecognitionTask?
    private var generation = 0
    private var latestText = ""
    private var consecutiveErrors = 0
    private var stopping = false

    init(recognizer: SFSpeechRecognizer, contextualTerms: [String]) {
        self.recognizer = recognizer
        self.contextualTerms = contextualTerms
    }

    func start() {
        controlQueue.sync {
            beginRecognition()
        }
    }

    func append(_ buffer: AVAudioPCMBuffer) {
        requestLock.lock()
        request?.append(buffer)
        requestLock.unlock()
    }

    func stop() {
        controlQueue.async { [weak self] in
            guard let self else { return }
            self.stopping = true
            self.generation += 1
            let previous = self.detachRequest()
            previous.request?.endAudio()
            previous.task?.finish()
        }
    }

    private func beginRecognition() {
        guard !stopping else { return }
        generation += 1
        let activeGeneration = generation
        latestText = ""

        let nextRequest = SFSpeechAudioBufferRecognitionRequest()
        nextRequest.shouldReportPartialResults = true
        nextRequest.taskHint = .dictation
        nextRequest.contextualStrings = contextualTerms
        if #available(macOS 13.0, *) {
            nextRequest.addsPunctuation = true
        }

        let nextTask = recognizer.recognitionTask(with: nextRequest) { [weak self] result, error in
            self?.controlQueue.async {
                self?.handle(result: result, error: error, generation: activeGeneration)
            }
        }
        requestLock.lock()
        request = nextRequest
        task = nextTask
        requestLock.unlock()

        // Apple imposes practical duration limits on recognition tasks. Rotate
        // the task while keeping AVAudioEngine alive so long sessions continue.
        controlQueue.asyncAfter(deadline: .now() + 50) { [weak self] in
            self?.rolloverIfCurrent(activeGeneration)
        }
    }

    private func handle(
        result: SFSpeechRecognitionResult?,
        error: Error?,
        generation activeGeneration: Int
    ) {
        guard !stopping, activeGeneration == generation else { return }
        if let result {
            let text = result.bestTranscription.formattedString
                .trimmingCharacters(in: .whitespacesAndNewlines)
            if !text.isEmpty {
                latestText = text
                consecutiveErrors = 0
                emit(["type": result.isFinal ? "final" : "partial", "text": text])
            }
            if result.isFinal {
                restart()
                return
            }
        }
        if let error {
            consecutiveErrors += 1
            if consecutiveErrors >= 3 {
                emit(["type": "error", "message": error.localizedDescription])
                exit(1)
            }
            restart(after: 0.3)
        }
    }

    private func rolloverIfCurrent(_ activeGeneration: Int) {
        guard !stopping, activeGeneration == generation else { return }
        if !latestText.isEmpty {
            emit(["type": "final", "text": latestText])
        }
        restart()
    }

    private func restart(after delay: TimeInterval = 0) {
        generation += 1
        let previous = detachRequest()
        previous.task?.cancel()
        previous.request?.endAudio()
        if delay > 0 {
            controlQueue.asyncAfter(deadline: .now() + delay) { [weak self] in
                self?.beginRecognition()
            }
        } else {
            beginRecognition()
        }
    }

    private func detachRequest() -> (
        request: SFSpeechAudioBufferRecognitionRequest?,
        task: SFSpeechRecognitionTask?
    ) {
        requestLock.lock()
        let previous = (request, task)
        request = nil
        task = nil
        requestLock.unlock()
        return previous
    }
}

private final class SystemAudioCapture: NSObject, SCStreamOutput, SCStreamDelegate {
    private let controller: RecognitionController
    private let sampleQueue = DispatchQueue(
        label: "realtime-translation.system-audio",
        qos: .userInitiated
    )
    private let stateLock = NSLock()
    private var stream: SCStream?
    private var stopping = false

    init(controller: RecognitionController) {
        self.controller = controller
    }

    func start(completion: @escaping (Error?) -> Void) {
        SCShareableContent.getExcludingDesktopWindows(
            false,
            onScreenWindowsOnly: false
        ) { [weak self] content, error in
            guard let self else { return }
            if let error {
                completion(error)
                return
            }
            guard let display = content?.displays.first else {
                completion(SystemAudioError.noDisplay)
                return
            }

            let filter = SCContentFilter(
                display: display,
                excludingApplications: [],
                exceptingWindows: []
            )
            let configuration = SCStreamConfiguration()
            configuration.capturesAudio = true
            configuration.excludesCurrentProcessAudio = true
            configuration.sampleRate = 48_000
            configuration.channelCount = 1
            // No screen frames are consumed, but keeping the configured surface
            // tiny avoids unnecessary WindowServer work for an audio-only stream.
            configuration.width = 2
            configuration.height = 2
            configuration.minimumFrameInterval = CMTime(value: 1, timescale: 1)
            configuration.queueDepth = 3

            let nextStream = SCStream(
                filter: filter,
                configuration: configuration,
                delegate: self
            )
            do {
                try nextStream.addStreamOutput(
                    self,
                    type: .audio,
                    sampleHandlerQueue: self.sampleQueue
                )
            } catch {
                completion(error)
                return
            }

            self.stateLock.lock()
            self.stream = nextStream
            let shouldStop = self.stopping
            self.stateLock.unlock()
            if shouldStop {
                nextStream.stopCapture()
                completion(SystemAudioError.stoppedBeforeStart)
                return
            }
            nextStream.startCapture { error in
                completion(error)
            }
        }
    }

    func stop() {
        stateLock.lock()
        stopping = true
        let activeStream = stream
        stream = nil
        stateLock.unlock()
        activeStream?.stopCapture()
    }

    func stream(
        _ stream: SCStream,
        didOutputSampleBuffer sampleBuffer: CMSampleBuffer,
        of outputType: SCStreamOutputType
    ) {
        guard outputType == .audio, sampleBuffer.isValid else { return }
        try? sampleBuffer.withAudioBufferList { audioBufferList, _ in
            guard let description = sampleBuffer.formatDescription?.audioStreamBasicDescription,
                  let format = AVAudioFormat(
                    standardFormatWithSampleRate: description.mSampleRate,
                    channels: description.mChannelsPerFrame
                  ),
                  let samples = AVAudioPCMBuffer(
                    pcmFormat: format,
                    bufferListNoCopy: audioBufferList.unsafePointer
                  ) else {
                return
            }
            controller.append(samples)
        }
    }

    func stream(_ stream: SCStream, didStopWithError error: Error) {
        stateLock.lock()
        let wasStopping = stopping
        self.stream = nil
        stateLock.unlock()
        if !wasStopping {
            emit(["type": "error", "message": "System audio capture stopped: \(error.localizedDescription)"])
        }
    }
}

private enum SystemAudioError: LocalizedError {
    case noDisplay
    case stoppedBeforeStart

    var errorDescription: String? {
        switch self {
        case .noDisplay:
            return "No display is available for system audio capture"
        case .stoppedBeforeStart:
            return "System audio capture was stopped before it started"
        }
    }
}

let localeIdentifier = argumentValues(named: "--locale").first ?? "zh-CN"
let contextualTerms = argumentValues(named: "--term")
let audioSourceValue = argumentValues(named: "--audio-source").first ?? "microphone"
guard let audioSource = AudioSource(rawValue: audioSourceValue) else {
    fail("Unsupported audio source: \(audioSourceValue)")
}

if audioSource == .microphone, !authorizeMicrophone() {
    fail(
        "Microphone permission was not granted. Open System Settings > Privacy & Security > Microphone and enable the app used to launch Realtime AI Translation."
    )
}

let authorization = DispatchSemaphore(value: 0)
var authorizationStatus = SFSpeechRecognizer.authorizationStatus()
if authorizationStatus == .notDetermined {
    SFSpeechRecognizer.requestAuthorization { status in
        authorizationStatus = status
        authorization.signal()
    }
    if authorization.wait(timeout: .now() + 30) == .timedOut {
        fail("Speech Recognition authorization timed out")
    }
}
guard authorizationStatus == .authorized else {
    fail(
        "Speech Recognition permission was not granted. Open System Settings > Privacy & Security > Speech Recognition and enable the app used to launch Realtime AI Translation."
    )
}

guard let recognizer = SFSpeechRecognizer(locale: Locale(identifier: localeIdentifier)) else {
    fail("Apple Speech does not support locale \(localeIdentifier)")
}
guard recognizer.isAvailable else {
    fail("Apple Speech is currently unavailable")
}

private let controller = RecognitionController(
    recognizer: recognizer,
    contextualTerms: contextualTerms
)
controller.start()
private var microphoneEngine: AVAudioEngine?
private var microphoneInput: AVAudioInputNode?
private var systemAudioCapture: SystemAudioCapture?

switch audioSource {
case .microphone:
    let engine = AVAudioEngine()
    let input = engine.inputNode
    let format = input.outputFormat(forBus: 0)
    input.installTap(onBus: 0, bufferSize: 1_024, format: format) { buffer, _ in
        controller.append(buffer)
    }
    do {
        engine.prepare()
        try engine.start()
        microphoneEngine = engine
        microphoneInput = input
        emit(["type": "ready", "locale": localeIdentifier])
    } catch {
        controller.stop()
        fail("Microphone capture failed: \(error.localizedDescription)")
    }
case .systemAudio:
    let capture = SystemAudioCapture(controller: controller)
    systemAudioCapture = capture
    capture.start { error in
        if let error {
            controller.stop()
            fail(
                "System audio capture failed: \(error.localizedDescription). Open System Settings > Privacy & Security > Screen & System Audio Recording, enable the app used to launch Realtime AI Translation, then restart it."
            )
        }
        emit(["type": "ready", "locale": localeIdentifier])
    }
}

DispatchQueue.global(qos: .userInitiated).async {
    while let command = readLine() {
        if command.trimmingCharacters(in: .whitespacesAndNewlines) == "stop" {
            microphoneEngine?.stop()
            microphoneInput?.removeTap(onBus: 0)
            systemAudioCapture?.stop()
            controller.stop()
            DispatchQueue.main.asyncAfter(deadline: .now() + 2.5) {
                exit(0)
            }
            return
        }
    }
}

RunLoop.current.run()
