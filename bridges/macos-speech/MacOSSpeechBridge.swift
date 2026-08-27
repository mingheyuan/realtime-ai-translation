import AVFoundation
import Foundation
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

let localeIdentifier = argumentValues(named: "--locale").first ?? "zh-CN"
let contextualTerms = argumentValues(named: "--term")

guard authorizeMicrophone() else {
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

let engine = AVAudioEngine()
private let controller = RecognitionController(
    recognizer: recognizer,
    contextualTerms: contextualTerms
)
let input = engine.inputNode
let format = input.outputFormat(forBus: 0)
input.installTap(onBus: 0, bufferSize: 1_024, format: format) { buffer, _ in
    controller.append(buffer)
}

controller.start()
do {
    engine.prepare()
    try engine.start()
} catch {
    controller.stop()
    fail("Microphone capture failed: \(error.localizedDescription)")
}

emit(["type": "ready", "locale": localeIdentifier])

DispatchQueue.global(qos: .userInitiated).async {
    while let command = readLine() {
        if command.trimmingCharacters(in: .whitespacesAndNewlines) == "stop" {
            engine.stop()
            input.removeTap(onBus: 0)
            controller.stop()
            DispatchQueue.main.asyncAfter(deadline: .now() + 2.5) {
                exit(0)
            }
            return
        }
    }
}

RunLoop.current.run()
