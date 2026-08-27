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

codesign_identity="${RT_TRANSLATION_CODESIGN_IDENTITY:-Realtime Translation Local Code Signing}"
if security find-identity -v -p codesigning | grep -Fq "\"$codesign_identity\""; then
  codesign --force --deep --sign "$codesign_identity" "$app_path"
else
  echo "warning: stable signing identity '$codesign_identity' was not found" >&2
  echo "warning: run scripts/setup-macos-codesigning.sh before granting macOS permissions" >&2
  codesign --force --deep --sign - "$app_path"
fi
echo "Built $app_path"
