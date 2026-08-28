#!/usr/bin/env python3
"""Sherpa-ONNX ASR bridge for Realtime AI Translation.

The process connects to the Rust control socket, lazily loads a bilingual
streaming model, and launches the signed macOS capture app in raw PCM mode.
It supports both microphone and ScreenCaptureKit system audio without making
Python the macOS privacy-permission owner.
"""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import threading
from typing import Optional
import uuid

import numpy as np


MODEL_NAME = "sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20"
MODEL_SAMPLE_RATE = 16_000


def arguments() -> argparse.Namespace:
    parser = argparse.ArgumentParser()
    parser.add_argument("--socket", required=True)
    parser.add_argument("--locale", default="zh-CN")
    parser.add_argument(
        "--audio-source", choices=("microphone", "system_audio"), default="microphone"
    )
    parser.add_argument("--term", action="append", default=[])
    return parser.parse_args()


class ControlChannel:
    def __init__(self, path: str) -> None:
        self.socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.socket.connect(path)
        self.lock = threading.Lock()
        self.stopping = threading.Event()
        self.reader = threading.Thread(target=self._read_commands, daemon=True)
        self.reader.start()

    def emit(self, event_type: str, **fields: object) -> None:
        payload = json.dumps(
            {"type": event_type, **fields}, ensure_ascii=False, separators=(",", ":")
        )
        with self.lock:
            self.socket.sendall(payload.encode("utf-8") + b"\n")

    def _read_commands(self) -> None:
        try:
            stream = self.socket.makefile("r", encoding="utf-8")
            for line in stream:
                if line.strip() == "stop":
                    self.stopping.set()
                    return
        except OSError:
            pass
        self.stopping.set()

    def close(self) -> None:
        try:
            self.socket.shutdown(socket.SHUT_RDWR)
        except OSError:
            pass
        self.socket.close()


def project_root() -> Path:
    return Path(__file__).resolve().parents[2]


def model_directory() -> Path:
    configured = os.environ.get("RT_TRANSLATION_SHERPA_MODEL_DIR")
    return Path(configured).expanduser() if configured else project_root() / "models" / MODEL_NAME


def capture_app() -> Path:
    configured = os.environ.get("RT_TRANSLATION_SPEECH_BRIDGE")
    return (
        Path(configured).expanduser()
        if configured
        else project_root() / "target" / "RealtimeTranslationSpeechBridge.app"
    )


def required_model_files(directory: Path) -> dict[str, Path]:
    return {
        "tokens": directory / "tokens.txt",
        "encoder": directory / "encoder-epoch-99-avg-1.int8.onnx",
        "decoder": directory / "decoder-epoch-99-avg-1.onnx",
        "joiner": directory / "joiner-epoch-99-avg-1.int8.onnx",
    }


def create_recognizer(model_files: dict[str, Path], has_hotwords: bool):
    try:
        import sherpa_onnx
    except ImportError as error:
        raise RuntimeError(
            "sherpa-onnx is not installed; run ./scripts/setup-sherpa-onnx.sh"
        ) from error

    return sherpa_onnx.OnlineRecognizer.from_transducer(
        tokens=str(model_files["tokens"]),
        encoder=str(model_files["encoder"]),
        decoder=str(model_files["decoder"]),
        joiner=str(model_files["joiner"]),
        num_threads=2,
        sample_rate=MODEL_SAMPLE_RATE,
        feature_dim=80,
        enable_endpoint_detection=True,
        rule1_min_trailing_silence=1.8,
        rule2_min_trailing_silence=0.8,
        rule3_min_utterance_length=8.0,
        decoding_method="modified_beam_search" if has_hotwords else "greedy_search",
        max_active_paths=4,
        hotwords_score=1.8,
        provider="cpu",
    )


def hotword_expression(terms: list[str]) -> str:
    # Sherpa accepts slash-separated hotwords. CJK characters can be supplied
    # directly. English BPE tokenization is model-dependent, so deterministic
    # glossary correction remains the fallback for terms that cannot be encoded.
    return "/".join(term.strip() for term in terms if term.strip())


def create_stream(recognizer, terms: list[str]):
    hotwords = hotword_expression(terms)
    if not hotwords:
        return recognizer.create_stream()
    try:
        return recognizer.create_stream(hotwords)
    except Exception:
        return recognizer.create_stream()


def launch_capture(args: argparse.Namespace, listener: socket.socket) -> subprocess.Popen:
    app = capture_app()
    if not app.is_dir():
        raise RuntimeError(f"signed audio capture app is missing: {app}")
    command = [
        "/usr/bin/open",
        "-n",
        "-W",
        "-a",
        str(app),
        "--args",
        "--socket",
        listener.getsockname(),
        "--locale",
        args.locale,
        "--audio-source",
        args.audio_source,
        "--mode",
        "pcm",
    ]
    return subprocess.Popen(
        command,
        stdin=subprocess.DEVNULL,
        stdout=subprocess.DEVNULL,
        stderr=None,
    )


def decode_audio(
    control: ControlChannel,
    recognizer,
    stream,
    capture_socket: socket.socket,
    locale: str,
) -> None:
    capture_socket.settimeout(10)
    received = bytearray()
    while b"\n" not in received:
        received.extend(capture_socket.recv(4096))
        if len(received) > 16_384:
            raise RuntimeError("native audio capture returned an oversized PCM header")
    header_line, pending = bytes(received).split(b"\n", 1)
    header = json.loads(header_line.decode("utf-8"))
    if header.get("type") != "ready" or header.get("encoding") != "float32le":
        raise RuntimeError("native audio capture returned an invalid PCM header")
    input_sample_rate = int(header.get("sample_rate", 0))
    if not 8_000 <= input_sample_rate <= 96_000:
        raise RuntimeError("native audio capture returned an unsupported sample rate")
    capture_socket.settimeout(0.2)
    control.emit("ready", locale=locale)

    last_result = ""
    audio_bytes = bytearray(pending)
    bytes_per_read = int(0.1 * input_sample_rate) * np.dtype(np.float32).itemsize
    while not control.stopping.is_set():
        try:
            data = capture_socket.recv(bytes_per_read)
        except (TimeoutError, socket.timeout):
            continue
        if not data:
            break
        audio_bytes.extend(data)
        usable = len(audio_bytes) - (len(audio_bytes) % np.dtype(np.float32).itemsize)
        if usable == 0:
            continue
        samples = np.frombuffer(audio_bytes[:usable], dtype="<f4").copy()
        del audio_bytes[:usable]
        stream.accept_waveform(input_sample_rate, samples)
        while recognizer.is_ready(stream):
            recognizer.decode_stream(stream)

        result = recognizer.get_result(stream).strip()
        if result and result != last_result:
            last_result = result
            control.emit("partial", text=result)

        if recognizer.is_endpoint(stream):
            if result:
                control.emit("final", text=result)
            recognizer.reset(stream)
            last_result = ""

    tail = np.zeros(int(0.5 * input_sample_rate), dtype=np.float32)
    stream.accept_waveform(input_sample_rate, tail)
    stream.input_finished()
    while recognizer.is_ready(stream):
        recognizer.decode_stream(stream)
    final_result = recognizer.get_result(stream).strip()
    if final_result:
        control.emit("final", text=final_result)


def main() -> int:
    args = arguments()
    control = ControlChannel(args.socket)
    capture_listener: Optional[socket.socket] = None
    capture_connection: Optional[socket.socket] = None
    capture_process: Optional[subprocess.Popen] = None
    capture_path = Path("/tmp") / f"rt-pcm-{uuid.uuid4().hex}.sock"
    try:
        files = required_model_files(model_directory())
        missing = [str(path) for path in files.values() if not path.is_file()]
        if missing:
            raise RuntimeError(
                "Sherpa-ONNX model is incomplete; run ./scripts/setup-sherpa-onnx.sh. "
                f"Missing: {', '.join(missing)}"
            )

        recognizer = create_recognizer(files, bool(args.term))
        stream = create_stream(recognizer, args.term)
        if control.stopping.is_set():
            return 0

        capture_listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        capture_listener.bind(str(capture_path))
        capture_listener.listen(1)
        capture_listener.settimeout(20)
        capture_process = launch_capture(args, capture_listener)
        capture_connection, _ = capture_listener.accept()
        decode_audio(control, recognizer, stream, capture_connection, args.locale)
        return 0
    except Exception as error:
        try:
            control.emit("error", message=f"Sherpa-ONNX: {error}")
        except OSError:
            pass
        return 1
    finally:
        if capture_connection is not None:
            try:
                capture_connection.sendall(b"stop\n")
            except OSError:
                pass
            capture_connection.close()
        if capture_listener is not None:
            capture_listener.close()
        capture_path.unlink(missing_ok=True)
        if capture_process is not None:
            try:
                capture_process.wait(timeout=4)
            except subprocess.TimeoutExpired:
                capture_process.terminate()
        control.close()


if __name__ == "__main__":
    sys.exit(main())
