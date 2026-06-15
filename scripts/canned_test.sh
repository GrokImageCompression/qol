#!/usr/bin/env bash
# qol canned-audio end-to-end tester.
#
# Streams a known audio file to Aavaaz exactly the way qol does → prints
# every segment Aavaaz returns. Lets us exercise the transport + ASR
# pipeline with zero mic / zero user input, deterministically.
#
# Default audio is the JFK speech sample shipped with WhisperLive
# (assets/jfk.flac). Pass any audio file (wav/flac/mp3/...) as the first
# argument, or a quoted sentence to synthesize via espeak-ng.
#
# Usage:
#   ./canned_test.sh                            # default: jfk.flac
#   ./canned_test.sh /path/to/audio.wav         # any audio file
#   ./canned_test.sh "synthesize this sentence" # leading word "synth:" forces TTS
#
# Environment overrides:
#   WS      WebSocket URL (default: ws://localhost:9090)
#   MODEL   Whisper model name (default: distil-large-v3)
#
# Exit codes:
#   0   at least one transcript segment received
#   1   no segments returned within the timeout

set -e
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
DEFAULT_AUDIO="${SCRIPT_DIR}/../assets/jfk.flac"
ARG="${1:-$DEFAULT_AUDIO}"
WS="${WS:-ws://localhost:9090}"
MODEL="${MODEL:-distil-large-v3}"
RAW=/tmp/qol-canned.f32

if [ -f "$ARG" ]; then
    echo "== preparing audio file: $ARG =="
    ffmpeg -y -loglevel error -i "$ARG" -ac 1 -ar 16000 -f f32le "$RAW"
else
    echo "== synthesizing speech: \"$ARG\" =="
    WAV=/tmp/qol-canned.wav
    espeak-ng -s 150 -v en-us -w "$WAV" "$ARG" >/dev/null 2>&1
    ffmpeg -y -loglevel error -i "$WAV" -ac 1 -ar 16000 -f f32le "$RAW"
fi
BYTES=$(stat -c%s "$RAW")
SAMPLES=$((BYTES/4))
SECS_X10=$((SAMPLES*10/16000))
echo "  $BYTES bytes of 16 kHz mono f32 PCM (~${SECS_X10}/10 s)"

echo
echo "== streaming to $WS (model=$MODEL) =="
PYAAVAAZ="${PYAAVAAZ:-/home/aaron/src/Aavaaz/aavaaz/.venv/bin/python}"
"$PYAAVAAZ" - "$WS" "$RAW" "$MODEL" <<'EOF'
import json, sys, time, websocket

ws_url, raw_path, model = sys.argv[1:4]

ws = websocket.create_connection(ws_url, timeout=5)
ws.send(json.dumps({
    "uid": "canned-test",
    "language": "en",
    "task": "transcribe",
    "model": model,
    "use_vad": True,
    "hotwords": "",
}))
print("  handshake sent")

# Stream the raw audio in ~64ms chunks (1024 f32 samples = 4096 bytes)
# matching the rubato chunk size qol emits.
CHUNK = 1024 * 4
data = open(raw_path, "rb").read()
sent = 0
for i in range(0, len(data), CHUNK):
    ws.send(data[i:i+CHUNK], opcode=websocket.ABNF.OPCODE_BINARY)
    sent += 1
    time.sleep(0.06)
print(f"  streamed {sent} chunks ({len(data)} bytes)")

# Drain segments for up to 10 seconds after sending finishes.
ws.settimeout(2.0)
segments = []
deadline = time.time() + 10
while time.time() < deadline:
    try:
        msg = ws.recv()
    except Exception:
        continue
    try:
        env = json.loads(msg)
    except Exception:
        continue
    for s in env.get("segments", []):
        if s.get("completed"):
            segments.append(s["text"].strip())
    if env.get("message") == "DISCONNECT":
        break

ws.close()
print()
print(f"== {len(segments)} completed segment(s) ==")
for s in segments:
    print(f"  -> {s}")
sys.exit(0 if segments else 1)
EOF
