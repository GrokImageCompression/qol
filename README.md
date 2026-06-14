# qol

Voice-to-text dictation overlay. Hold a hotkey, talk, release — your cleaned-up
words appear in whatever app has focus. Powered by
[Aavaaz](../Aavaaz/aavaaz/) for streaming transcription and an LLM polish pass
for filler removal, punctuation, and per-app tone.

`qol` (қол) means "voice" in Kazakh — a sibling name to Aavaaz ("voice" in Hindi).

## Architecture

```
┌──────────────────────────────────────────────────┐
│  qol (Tauri desktop app)                         │
│                                                  │
│  global-shortcut ──► push-to-talk                │
│  cpal             ──► 16 kHz mono PCM            │
│  tokio-tungstenite──► ws://localhost:9090        │  ── Aavaaz/WhisperLive ──► transcript
│  active-win       ──► focused app context        │
│  Claude API       ──► polish (tone, punctuation) │
│  enigo            ──► inject text into focused app│
│                                                  │
│  webview          ──► settings UI (Vite + TS)    │
└──────────────────────────────────────────────────┘
```

## Layout

| Path | Purpose |
|---|---|
| `src-tauri/Cargo.toml` | Rust deps (tauri, cpal, tokio-tungstenite, enigo, reqwest) |
| `src-tauri/src/main.rs` | App entry, hotkey wiring, Tauri commands |
| `src-tauri/src/audio.rs` | Mic capture → 16 kHz mono f32 frames |
| `src-tauri/src/transport.rs` | WebSocket session to Aavaaz/WhisperLive |
| `src-tauri/src/session.rs` | Lifecycle: audio → transport → polish → inject |
| `src-tauri/src/inject.rs` | Keystroke injection (enigo) + active-window probe |
| `src-tauri/src/polish.rs` | Claude API call for transcript cleanup |
| `src-tauri/src/config.rs` | JSON config in `~/.config/qol/config.json` |
| `index.html` + `src/` | Vite settings UI |

## Prerequisites

- Rust 1.77+ (`rustup toolchain install stable`)
- Node 20+ (`pnpm` or `npm`)
- A running Aavaaz instance at `ws://localhost:9090`
- Optional LLM polish: any **OpenAI-compatible** endpoint —
  - `OPENAI_API_KEY` env var for OpenAI (default)
  - Or point `base_url` at Groq, OpenRouter, Together, Cerebras, Mistral, ...
  - Or **fully local**: Ollama (`http://localhost:11434/v1`, model `qwen2.5:7b-instruct`)
    or llama.cpp's `--server` (`http://localhost:8080/v1`) — leave the API key
    env var empty
  - Or skip entirely: with polish disabled, raw transcripts inject fine

### Fedora-specific system deps

```bash
sudo dnf install -y \
  webkit2gtk4.1-devel \
  openssl-devel \
  curl wget file \
  libappindicator-gtk3-devel \
  librsvg2-devel \
  gtk3-devel \
  alsa-lib-devel \
  libxdo-devel
```

`libxdo-devel` is needed by `enigo` on X11.

### Wayland (GNOME, KDE Plasma 6 in Wayland mode)

GNOME Wayland blocks synthetic X11 input, so we automatically detect Wayland
and route injection through `ydotool`. Setup:

```bash
sudo dnf install ydotool   # or: apt install ydotool

# uinput needs to be readable by your user — easiest via udev rule:
echo 'KERNEL=="uinput", MODE="0660", GROUP="input"' | \
  sudo tee /etc/udev/rules.d/80-uinput.rules
sudo udevadm control --reload && sudo udevadm trigger
sudo usermod -aG input "$USER"   # log out + back in

# Start the daemon (usually as a systemd user unit):
systemctl --user enable --now ydotoold
```

qol picks the backend automatically at startup. Look for
`selected injection backend = Ydotool` in the logs to confirm.

## Run

```bash
# in one terminal — start Aavaaz with a model that fits your GPU
cd ../Aavaaz/aavaaz
source .venv/bin/activate
aavaaz serve --model distil-large-v3

# in another — build and run qol
cd ../../qol
npm install
npm run tauri dev
```

Then press your hotkey (default `Super+Space`), speak, and release.

## Settings

Edit via the settings window (open from system tray), or directly at
`~/.config/qol/config.json`:

```json
{
  "aavaaz_url": "ws://localhost:9090",
  "model": "distil-large-v3",
  "language": "en",
  "hotkey": "Super+Space",
  "polish": {
    "enabled": true,
    "base_url": "https://api.openai.com/v1",
    "model": "gpt-4o-mini",
    "api_key_env": "OPENAI_API_KEY",
    "per_app_tone": true
  },
  "hotwords": ["Aavaaz", "qol", "WhisperLive"],
  "inject_method": "type"
}
```

## Tests & CI

```bash
cd src-tauri
cargo test          # unit tests (config round-trip, hotkey parser)
cargo clippy --all-targets -- -D warnings
cargo fmt -- --check
```

CI runs the above on **Ubuntu, macOS, and Windows** for every push and PR — see
[.github/workflows/ci.yml](.github/workflows/ci.yml).

Hardware-bound paths (audio capture, keystroke injection) and network-bound
paths (WebSocket session, Claude polish) aren't unit-tested yet; integration
tests with a fake Aavaaz endpoint and a virtual audio device are a TODO.

## Status

This is a scaffold. Working / stubbed:

- [x] Mic capture with `rubato` polyphase resampling to 16 kHz
- [x] WebSocket session with Aavaaz/WhisperLive handshake
- [x] LLM polish pass with per-app tone hint
- [x] Streaming injection (each completed segment types as it arrives)
- [x] Voice commands: `scratch that`, `new line`, `new paragraph`, `select all`
- [x] Linux injection backend selector: `enigo` on X11, `ydotool` on Wayland
- [x] Global hotkey via `tauri-plugin-global-shortcut`
- [x] Tray menu (open settings, pause/resume, quit)
- [x] Settings UI
- [ ] Per-app tone profiles configurable in UI
- [ ] Tone-rolling-context across segments (consistency in long dictation)
- [ ] Local-only polish via llama.cpp
- [ ] Integration tests with a fake Aavaaz WS server

## License

MPL-2.0
