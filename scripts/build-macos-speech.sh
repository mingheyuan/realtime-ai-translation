#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd "$(dirname "$0")/.." && pwd)"
app_path="$project_dir/target/RealtimeTranslationSpeechBridge.app"
contents_path="$app_path/Contents"
output_path="$contents_path/MacOS/macos-speech-bridge"
mkdir -p "$contents_path/MacOS"
module_cache="$project_dir/target/swift-module-cache"
mkdir -p "$module_cache"

cp "$project_dir/bridges/macos-speech/Info.plist" "$contents_path/Info.plist"

swiftc \
  "$project_dir/bridges/macos-speech/MacOSSpeechBridge.swift" \
  -module-cache-path "$module_cache" \
  -framework AVFoundation \
  -framework CoreMedia \
  -framework ScreenCaptureKit \
  -framework Speech \
  -o "$output_path"

codesign --force --deep --sign - "$app_path"
echo "Built $app_path"
