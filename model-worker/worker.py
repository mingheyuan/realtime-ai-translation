#!/usr/bin/env python3
"""Persistent MarianMT worker using newline-delimited JSON over stdin/stdout."""

import json
import os
import re
import sys
import time
from typing import Any, Dict, List, Tuple


MODEL_IDS = {
    ("zh", "en"): "Helsinki-NLP/opus-mt-zh-en",
    ("en", "zh"): "Helsinki-NLP/opus-mt-en-zh",
}

MODELS: Dict[Tuple[str, str], Tuple[Any, Any, str]] = {}


def base_language(value: str) -> str:
    return value.lower().replace("_", "-").split("-", 1)[0]


def load_model(source_language: str, target_language: str) -> Tuple[Any, Any, str]:
    direction = (base_language(source_language), base_language(target_language))
    if direction not in MODEL_IDS:
        raise ValueError("only Chinese-English translation is supported")
    if direction in MODELS:
        return MODELS[direction]

    from huggingface_hub import snapshot_download
    from transformers import AutoModelForSeq2SeqLM, AutoTokenizer
    import torch

    model_id = MODEL_IDS[direction]
    requested_device = os.getenv("RT_TRANSLATION_MT_DEVICE", "cpu").lower()
    device = "mps" if requested_device == "mps" and torch.backends.mps.is_available() else "cpu"
    snapshot_path = snapshot_download(
        repo_id=model_id,
        allow_patterns=["*.json", "*.spm", "pytorch_model.bin"],
    )
    tokenizer = AutoTokenizer.from_pretrained(snapshot_path, local_files_only=True)
    # Loading from the downloaded local snapshot prevents Transformers from
    # launching a background safetensors conversion for these PyTorch weights.
    model = AutoModelForSeq2SeqLM.from_pretrained(
        snapshot_path,
        local_files_only=True,
        use_safetensors=False,
    )
    model.eval()
    model.to(device)
    MODELS[direction] = (tokenizer, model, device)
    return MODELS[direction]


def exact_glossary(text: str, glossary: List[Dict[str, Any]]) -> str:
    normalized = text.strip().casefold()
    for entry in glossary:
        candidates = [entry.get("source", ""), *entry.get("aliases", [])]
        if any(candidate.strip().casefold() == normalized for candidate in candidates if candidate):
            return str(entry.get("target", "")).strip()
    return ""


def protect_glossary(
    text: str, glossary: List[Dict[str, Any]]
) -> Tuple[str, List[Tuple[str, str]]]:
    protected = text
    replacements: List[Tuple[str, str]] = []
    sorted_entries = sorted(
        glossary,
        key=lambda entry: len(str(entry.get("source", ""))),
        reverse=True,
    )
    for index, entry in enumerate(sorted_entries):
        source = str(entry.get("source", "")).strip()
        target = str(entry.get("target", "")).strip()
        if not source or not target:
            continue
        token = "ZXQGLOSSARY{}QXZ".format(index)
        candidates = [source, *entry.get("aliases", [])]
        changed = False
        for candidate in candidates:
            if not candidate:
                continue
            protected, count = re.subn(
                re.escape(str(candidate)), token, protected, flags=re.IGNORECASE
            )
            changed = changed or count > 0
        if changed:
            replacements.append((token, target))
    return protected, replacements


def restore_glossary(text: str, replacements: List[Tuple[str, str]]) -> str:
    restored = text
    for token, target in replacements:
        restored = re.sub(re.escape(token), target, restored, flags=re.IGNORECASE)
        spaced = " ".join(token)
        restored = re.sub(re.escape(spaced), target, restored, flags=re.IGNORECASE)
    return restored


def translate(request: Dict[str, Any]) -> Dict[str, Any]:
    started = time.perf_counter()
    text = str(request.get("text", "")).strip()
    if not text:
        raise ValueError("text is required")
    source_language = str(request.get("source_language", ""))
    target_language = str(request.get("target_language", ""))
    glossary = request.get("glossary", [])
    exact = exact_glossary(text, glossary)
    if exact:
        return {
            "id": request.get("id"),
            "translation": exact,
            "latency_ms": int((time.perf_counter() - started) * 1000),
        }

    protected, replacements = protect_glossary(text, glossary)
    tokenizer, model, device = load_model(source_language, target_language)
    import torch

    inputs = tokenizer(protected, return_tensors="pt", truncation=True, max_length=512)
    inputs = {key: value.to(device) for key, value in inputs.items()}
    with torch.inference_mode():
        generated = model.generate(
            **inputs,
            max_new_tokens=256,
            num_beams=1,
            do_sample=False,
        )
    result = tokenizer.batch_decode(generated, skip_special_tokens=True)[0].strip()
    result = restore_glossary(result, replacements)
    if not result:
        raise RuntimeError("translation model returned empty text")
    return {
        "id": request.get("id"),
        "translation": result,
        "latency_ms": int((time.perf_counter() - started) * 1000),
    }


def main() -> None:
    for line in sys.stdin:
        try:
            request = json.loads(line)
            response = translate(request)
        except Exception as error:  # The Rust boundary receives a structured fallback reason.
            response = {
                "id": request.get("id") if "request" in locals() else None,
                "error": "{}: {}".format(type(error).__name__, error),
            }
        sys.stdout.write(json.dumps(response, ensure_ascii=False) + "\n")
        sys.stdout.flush()


if __name__ == "__main__":
    main()
