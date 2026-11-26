#!/bin/sh
# Baseline measurements in nested mode (a window on the current desktop).
#   scripts/baseline-nested.sh [OUTPUT_DIR]
# Takes about 11 minutes. Uses the release build in target/release.
#
# Measures: idle CPU over 60 s, memory after 1 and 10 minutes, CPU while
# locked. It cannot move windows or unlock: nothing here can inject input.
set -u
out=${1:-/tmp/meridian-baseline}
mkdir -p "$out"
cd "$(dirname "$0")/.." || exit 1
shell="meridian-comp meridian-bar meridian-dock meridian-desktop meridian-launch meridian-notify"

MERIDIAN_STATS="$out/stats.csv" ./target/release/meridian-comp --winit alacritty > "$out/comp.log" 2>&1 &
comp=$!
sleep 10
echo "== idle, 60 s (one terminal open, nothing moving)" | tee "$out/summary.txt"
scripts/cpu-sample.sh 60 $shell alacritty | tee -a "$out/summary.txt"
echo "== memory after 10 minutes" | tee -a "$out/summary.txt"
sleep 520
scripts/cpu-sample.sh 10 $shell | tee -a "$out/summary.txt"
echo "== locked, 15 s" | tee -a "$out/summary.txt"
sock="$XDG_RUNTIME_DIR/meridian-$(grep -o 'WAYLAND_DISPLAY=[a-z0-9-]*' "$out/comp.log" | head -1 | cut -d= -f2).sock"
python3 -c 'import socket,sys; s=socket.socket(socket.AF_UNIX); s.connect(sys.argv[1]); s.sendall(b"{\"action\":\"lock\"}\n")' "$sock"
sleep 5
scripts/cpu-sample.sh 15 meridian-comp meridian-lock | tee -a "$out/summary.txt"
kill "$comp"
echo "== compositor statistics (per second) are in $out/stats.csv" | tee -a "$out/summary.txt"
