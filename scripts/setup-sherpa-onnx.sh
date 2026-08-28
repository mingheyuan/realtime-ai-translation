#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd "$(dirname "$0")/.." && pwd)"
venv_path="$project_dir/.venv"
model_name="sherpa-onnx-streaming-zipformer-bilingual-zh-en-2023-02-20"
model_root="$project_dir/models"
model_path="$model_root/$model_name"
archive_path="$model_root/$model_name.tar.bz2"
download_url="https://github.com/k2-fsa/sherpa-onnx/releases/download/asr-models/$model_name.tar.bz2"

if [[ ! -x "$venv_path/bin/python" ]]; then
  python3 -m venv "$venv_path"
fi

"$venv_path/bin/pip" install -r "$project_dir/bridges/sherpa-onnx/requirements.txt"

required_files=(
  "tokens.txt"
  "encoder-epoch-99-avg-1.int8.onnx"
  "decoder-epoch-99-avg-1.onnx"
  "joiner-epoch-99-avg-1.int8.onnx"
)

complete=1
for filename in "${required_files[@]}"; do
  if [[ ! -f "$model_path/$filename" ]]; then
    complete=0
  fi
done

if [[ "$complete" -eq 0 ]]; then
  mkdir -p "$model_root"
  curl -fL --retry 3 -o "$archive_path" "$download_url"
  tar -xjf "$archive_path" -C "$model_root"
  rm "$archive_path"

  # Keep only the INT8 inference set used by the bridge.
  find "$model_path" -maxdepth 1 -type f -name '*.onnx' \
    ! -name 'encoder-epoch-99-avg-1.int8.onnx' \
    ! -name 'decoder-epoch-99-avg-1.onnx' \
    ! -name 'joiner-epoch-99-avg-1.int8.onnx' \
    -delete
fi

chmod +x "$project_dir/scripts/sherpa-onnx-bridge"
echo "Sherpa-ONNX is ready: $model_path"
