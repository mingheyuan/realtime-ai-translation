#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd "$(dirname "$0")/.." && pwd)"
output_path="$project_dir/target/macos-speech-bridge"
mkdir -p "$project_dir/target"
module_cache="$project_dir/target/swift-module-cache"
mkdir -p "$module_cache"

swiftc \
  "$project_dir/bridges/macos-speech/MacOSSpeechBridge.swift" \
  -module-cache-path "$module_cache" \
  -framework AVFoundation \
  -framework CoreMedia \
  -framework ScreenCaptureKit \
  -framework Speech \
  -Xlinker -sectcreate \
  -Xlinker __TEXT \
  -Xlinker __info_plist \
  -Xlinker "$project_dir/bridges/macos-speech/Info.plist" \
  -o "$output_path"

codesign --force --sign - "$output_path"
echo "Built $output_path"
