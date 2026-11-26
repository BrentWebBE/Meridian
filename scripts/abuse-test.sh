#!/bin/sh
# Abuse test: things that must not take the compositor down.
#   scripts/abuse-test.sh
# Starts a nested compositor (release build), mistreats it, and prints
# PASS / FAIL per check. Exit status is the number of FAILs.
# The lock part needs /etc/pam.d/meridian-lock (installed by the package).
set -u
cd "$(dirname "$0")/.." || exit 1
work=$(mktemp -d)
fails=0
pass() { printf '  PASS  %s\n' "$1"; }
fail() { printf '  FAIL  %s\n' "$1"; fails=$((fails + 1)); }
alive() { kill -0 "$comp" 2>/dev/null; }
check() { if alive; then pass "$1"; else fail "$1 (compositor died)"; fi; }

mkdir -p "$work/config/meridian"
XDG_CONFIG_HOME="$work/config" XDG_STATE_HOME="$work/state" \
    ./target/release/meridian-comp --winit alacritty > "$work/comp.log" 2>&1 &
comp=$!
sleep 5
display=$(grep -o 'WAYLAND_DISPLAY=[a-z0-9-]*' "$work/comp.log" | head -1 | cut -d= -f2)
wayland="$XDG_RUNTIME_DIR/$display"
ipc="$XDG_RUNTIME_DIR/meridian-$display.sock"
state() { python3 -c '
import socket, sys
s = socket.socket(socket.AF_UNIX); s.settimeout(2); s.connect(sys.argv[1])
if len(sys.argv) > 2: s.sendall(sys.argv[2].encode() + b"\n")
import time; time.sleep(0.5)
print(s.recv(65536).decode().strip().split("\n")[-1])' "$ipc" "$@" 2>/dev/null; }

echo "Bad input"
python3 -c '
import os, socket, sys
for i in range(30):
    s = socket.socket(socket.AF_UNIX); s.connect(sys.argv[1])
    s.sendall(os.urandom(4096)); s.close()' "$wayland"
check "30 clients sending random bytes on the Wayland socket"
python3 -c '
import os, socket, sys
for payload in (os.urandom(4096), b"{not json}\n", b"x" * (2 << 20), b"{\"action\":\"no-such-action\"}\n", b"\n\n\n"):
    s = socket.socket(socket.AF_UNIX); s.connect(sys.argv[1])
    try: s.sendall(payload)
    except OSError: pass
    s.close()' "$ipc"
check "garbage, invalid JSON and a 2 MB line on the IPC socket"
if state | grep -q '"windows"'; then pass "IPC still answers a well-behaved client"
else fail "IPC no longer answers"; fi

echo "Dying clients"
{ for round in 1 2 3 4 5; do
    WAYLAND_DISPLAY=$display alacritty > /dev/null 2>&1 &
    victim=$!
    sleep 0.7
    kill -9 "$victim" 2>/dev/null
    wait "$victim"
done; } 2>/dev/null   # hide the shell's own "Killed" notices
sleep 0.5
check "5 windows whose client was killed with SIGKILL"

echo "Shell programs"
app=$(pgrep -n -x alacritty)
for name in meridian-bar meridian-dock meridian-deskto meridian-launch; do
    pid=$(pgrep -n -x "$name")
    if [ -z "$pid" ]; then fail "$name was not running"; continue; fi
    kill -9 "$pid"
    sleep 0.4
    check "$name killed"
done
if kill -0 "$app" 2>/dev/null; then pass "the open app survived all of it"
else fail "the open app died"; fi

echo "Lock screen"
if [ -f /etc/pam.d/meridian-lock ]; then
    state '{"action":"lock"}' > /dev/null
    sleep 4
    if state | grep -q '"locked":true'; then pass "session locks"; else fail "session did not lock"; fi
    pkill -9 -x meridian-lock
    sleep 0.5
    if state | grep -q '"locked":true'; then pass "still locked right after the lock program was killed"
    else fail "session unlocked when the lock program died"; fi
    sleep 4
    if pgrep -x meridian-lock > /dev/null && state | grep -q '"locked":true'; then
        pass "lock program restarted, session still locked"
    else fail "lock program did not come back"; fi
    state '{"focus-window":{"id":1}}' > /dev/null
    if state | grep -q '"focused":true'; then fail "a window got focus while locked"
    else pass "no window can take focus while locked"; fi
    check "compositor alive after the lock tests"
else
    echo "  SKIP  /etc/pam.d/meridian-lock is missing"
fi

if ls "$work/state/meridian"/panic-*.log > /dev/null 2>&1; then fail "a crash report was written"
else pass "no crash report"; fi
kill "$comp" 2>/dev/null
rm -rf "$work"
printf '\n%s check(s) failed.\n' "$fails"
exit "$fails"
