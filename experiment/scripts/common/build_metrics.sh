#!/usr/bin/env bash

run_build_with_metrics() {
    local log="$1"
    local baseline="$2"
    local dataset="$3"
    local threads="$4"
    local artifact="$5"
    shift 5

    mkdir -p "$(dirname "$log")"
    mkdir -p "$(dirname "$artifact")"

    local start end rc
    start=$(date +%s.%N)
    {
        echo "baseline,dataset,threads,artifact,start_time"
        echo "$baseline,$dataset,$threads,$artifact,$start"
        echo "command: $*"
    } > "$log"

    "$@" 2>&1 | tee -a "$log"
    rc=${PIPESTATUS[0]}
    end=$(date +%s.%N)

    python3 - "$start" "$end" "$rc" "$artifact" >> "$log" <<'PY'
import os
import sys

start = float(sys.argv[1])
end = float(sys.argv[2])
rc = int(sys.argv[3])
artifact = sys.argv[4]

if os.path.isdir(artifact):
    size = 0
    for root, _, files in os.walk(artifact):
        for name in files:
            path = os.path.join(root, name)
            if os.path.exists(path):
                size += os.path.getsize(path)
elif os.path.exists(artifact):
    size = os.path.getsize(artifact)
else:
    size = 0

print(f"elapsed_s,{end - start:.6f}")
print(f"exit_code,{rc}")
print(f"artifact_size_bytes,{size}")
PY

    return "$rc"
}
