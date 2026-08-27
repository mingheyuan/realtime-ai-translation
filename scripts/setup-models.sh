#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd "$(dirname "$0")/.." && pwd)"
python3 -m venv "$project_dir/.venv"
"$project_dir/.venv/bin/python" -m pip install --upgrade pip
"$project_dir/.venv/bin/python" -m pip install -r "$project_dir/model-worker/requirements.txt"

echo "Model worker environment is ready. Models download lazily on first use."

