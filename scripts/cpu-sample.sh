#!/bin/sh
# Average CPU use and memory of processes over a period.
#   scripts/cpu-sample.sh SECONDS NAME...
# NAME is a process name as shown by `ps -o comm` (first 15 characters).
# CPU is read from /proc/PID/stat (user + system ticks), so it is exact for
# the period and independent of the compositor's own statistics.
seconds=$1; shift
ticks() { awk '{ sub(/.*\) /, ""); print $12 + $13 }' "/proc/$1/stat" 2>/dev/null; }
for name in "$@"; do
    for pid in $(pgrep -x "$name"); do echo "$name $pid $(ticks "$pid")"; done
done > /tmp/cpu-sample.$$
sleep "$seconds"
while read -r name pid before; do
    after=$(ticks "$pid") || continue
    [ -n "$after" ] || continue
    rss=$(awk '/VmRSS/ { print $2 }' "/proc/$pid/status")
    # 100 ticks per second on Linux.
    awk -v n="$name" -v p="$pid" -v d="$((after - before))" -v s="$seconds" -v r="$rss" \
        'BEGIN { printf "%-18s pid %-8s cpu %5.2f%%  rss %6.1f MB\n", n, p, d / s, r / 1024 }'
done < /tmp/cpu-sample.$$
rm -f /tmp/cpu-sample.$$
