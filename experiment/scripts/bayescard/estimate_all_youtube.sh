#!/bin/bash
set -eu
set -o pipefail
workspace=$(realpath "$(dirname "$0")/../..")
python3 "$workspace/scripts/run_youtube_8baselines.py" estimate bayescard "$@"
