#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd "$(dirname "$0")/.." && pwd)"
app_path="$project_dir/target/RealtimeTranslationOverlay.app"
contents_path="$app_path/Contents"
output_path="$contents_path/MacOS/realtime-translation-overlay"
mkdir -p "$contents_path/MacOS"
module_cache="$project_dir/target/swift-module-cache"
mkdir -p "$module_cache"

cp "$project_dir/bridges/macos-overlay/Info.plist" "$contents_path/Info.plist"

swiftc \
  "$project_dir/bridges/macos-overlay/RealtimeTranslationOverlay.swift" \
  -module-cache-path "$module_cache" \
  -framework AppKit \
  -framework WebKit \
  -o "$output_path"

codesign --force --deep --sign - "$app_path"
echo "Built $app_path"
