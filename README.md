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
- An `ANTHROPIC_API_KEY` env var if you want the polish pass (optional — raw transcripts inject fine without it)

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

`libxdo-devel` is needed by `enigo` on X11. On Wayland, you may also need a
`ydotool` daemon for reliable injection (GNOME Wayland blocks synthetic input
otherwise).

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
    "provider": "anthropic",
    "model": "claude-haiku-4-5-20251001",
    "api_key_env": "ANTHROPIC_API_KEY",
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

- [x] Mic capture with cheap linear resampling to 16 kHz
- [x] WebSocket session with Aavaaz/WhisperLive handshake
- [x] LLM polish pass with per-app tone hint
- [x] enigo-based text injection
- [x] Global hotkey via `tauri-plugin-global-shortcut`
- [x] Settings UI
- [ ] Proper resampling (replace linear with `rubato`)
- [ ] Wayland injection fallback (`ydotool` IPC)
- [ ] Streaming injection (type as segments arrive vs. only at release)
- [ ] Per-app tone profiles configurable in UI
- [ ] Voice commands ("new line", "scratch that")
- [ ] Local-only polish via llama.cpp
- [ ] Tray menu (pause, open settings, quit)

## License

MPL-2.0
