#!/usr/bin/env bash
# Nuke both services + restart cleanly + verify they actually accept
# connections + tail the qol log live. ONE button.
#
# Defaults to distil-large-v3 (~3 GB VRAM) because large-v3 (~6 GB) OOMs
# on a 6 GB GPU as soon as a second client connects (each client tries
# to load its own model copy with WhisperLive's default single_model=False).
#
# Usage:
#   ./scripts/restart_all.sh
#   MODEL=large-v3 ./scripts/restart_all.sh   # if you have ≥8 GB VRAM
#
# After it prints "BOTH HEALTHY", press Ctrl+Alt+Space and dictate.
# Transcribed segments scroll in this same terminal as they arrive.

set +e
say()  { printf '\n\033[1;36m== %s ==\033[0m\n' "$*"; }
ok()   { printf '\033[1;32m  OK\033[0m  %s\n' "$*"; }
bad()  { printf '\033[1;31m FAIL\033[0m  %s\n' "$*"; }

AAVAAZ_DIR="$HOME/src/Aavaaz/aavaaz"
QOL_BIN="$HOME/src/qol/src-tauri/target/debug/qol"
AAVAAZ_LOG=/tmp/aavaaz.log
QOL_LOG=/tmp/qol.log
MODEL="${MODEL:-distil-large-v3}"

# ──────────────────────────── 1. KILL EVERYTHING ────────────────────────────
say "killing aavaaz + qol"
for pid in $(pgrep -f 'aavaaz serve'); do kill -9 "$pid" 2>/dev/null; done
for pid in $(pgrep -f 'target/debug/qol$'); do kill -9 "$pid" 2>/dev/null; done
sleep 2
rm -f "$XDG_RUNTIME_DIR/qol.sock"

if ss -tln | grep -qE ":9090|:8000"; then
    bad "ports :9090 or :8000 still LISTEN after kill"
    ss -tlnp | grep -E ":9090|:8000"
    exit 1
fi
ok "killed, ports free, socket gone"

# ──────────────────────────── 2. START AAVAAZ ────────────────────────────
say "starting aavaaz (model=$MODEL)"
: > "$AAVAAZ_LOG"
( cd "$AAVAAZ_DIR" && nohup .venv/bin/aavaaz serve --model "$MODEL" \
    > "$AAVAAZ_LOG" 2>&1 & disown )

# Wait for :9090 to LISTEN
for i in $(seq 1 30); do
    if ss -tln | grep -q ":9090"; then break; fi
    sleep 1
done
if ! ss -tln | grep -q ":9090"; then
    bad "aavaaz never opened :9090 — last 30 lines of its log:"
    tail -30 "$AAVAAZ_LOG"
    exit 1
fi
ok ":9090 listening"

# Probe ONCE with a long timeout — opening many probe connections triggers
# repeated model loads (~30 s each) and OOMs the GPU on small cards.
say "probing aavaaz WS (single attempt, up to 90 s for first model load)"
PROBE_OUT=$("$AAVAAZ_DIR/.venv/bin/python" - "$MODEL" <<'PY' 2>&1
import sys, json, time, websocket
model = sys.argv[1]
t = time.time()
try:
    ws = websocket.create_connection("ws://localhost:9090", timeout=10)
    ws.send(json.dumps({"uid":"probe","language":"en","task":"transcribe",
                        "model":model,"use_vad":True,"hotwords":""}))
    ws.settimeout(90)
    msg = ws.recv()
    ws.close()
    if "SERVER_READY" in msg:
        print(f"  SERVER_READY in {time.time()-t:.1f}s: {msg[:120]}")
        sys.exit(0)
    print(f"  unexpected msg: {msg[:200]}")
    sys.exit(2)
except Exception as e:
    print(f"  ERROR after {time.time()-t:.1f}s: {e}")
    sys.exit(1)
PY
)
PROBE_RC=$?
echo "$PROBE_OUT"
if [ "$PROBE_RC" -ne 0 ]; then
    bad "aavaaz WS handshake didn't return SERVER_READY"
    echo
    echo "=== aavaaz log tail (look for OOM / cublas / cudnn / traceback) ==="
    tail -40 "$AAVAAZ_LOG"
    exit 1
fi
ok "aavaaz responsive"

# ──────────────────────────── 3. START QOL ────────────────────────────
say "starting qol (debug logs)"
: > "$QOL_LOG"
RUST_LOG=qol=debug,info,warn nohup "$QOL_BIN" > "$QOL_LOG" 2>&1 &
disown

for i in $(seq 1 10); do
    if [ -S "$XDG_RUNTIME_DIR/qol.sock" ]; then break; fi
    sleep 1
done
if ! [ -S "$XDG_RUNTIME_DIR/qol.sock" ]; then
    bad "qol socket never appeared — log tail:"
    tail -20 "$QOL_LOG"
    exit 1
fi

STATUS=$("$QOL_BIN" status 2>&1)
if [[ "$STATUS" != STATUS* ]]; then
    bad "qol status can't reach daemon: $STATUS"
    exit 1
fi
ok "qol up, daemon $STATUS"

# ──────────────────────────── 4. LIVE TAIL ────────────────────────────
say "BOTH HEALTHY — talk now"
echo "  aavaaz log: $AAVAAZ_LOG"
echo "  qol log:    $QOL_LOG"
echo
echo "Press Ctrl+Alt+Space, talk, press it again to stop."
echo "Live output below. Ctrl+C will stop aavaaz + qol and exit."
echo "------------------------------------------------------------"

cleanup() {
    echo
    say "Ctrl+C — killing aavaaz + qol"
    pkill -9 -f 'aavaaz serve' 2>/dev/null
    pkill -9 -f 'target/debug/qol$' 2>/dev/null
    rm -f "$XDG_RUNTIME_DIR/qol.sock"
    exit 0
}
trap cleanup INT TERM

tail -n0 -F "$QOL_LOG" \
  | sed -E -u 's/\x1b\[[0-9;]*m//g' \
  | grep --line-buffered -E "session started|session stopped|parsed segment.*completed=true|transport session ended|polish failed|ERROR" \
  | sed -E -u 's/.*parsed segment.*text=/  → /
              s/.*session started.*/▶ recording/
              s/.*session stopped.*/■ stopped/' &
TAIL_PID=$!
wait "$TAIL_PID"
