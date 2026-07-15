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
│  OpenAI API       ──► polish (tone, punctuation) │
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
| `src-tauri/src/polish.rs` | OpenAI-compatible polish call for transcript cleanup |
| `src-tauri/src/config.rs` | JSON config in `~/.config/qol/config.json` |
| `index.html` + `src/` | Vite settings UI |

## Prerequisites

- Rust 1.77+ (`rustup toolchain install stable`)
- Node 20+ (`pnpm` or `npm`)
- A running Aavaaz instance at `ws://localhost:9090`
- Optional LLM polish: any **OpenAI-compatible** endpoint —
  - Store the key in the OS keyring via the settings window (checked first,
    keyed by `base_url`), or set an env var as fallback — see [Settings](#settings)
  - `OPENAI_API_KEY` env var for OpenAI (default fallback name)
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
and route injection through `ydotool`.

```bash
sudo dnf install ydotool   # or: apt install ydotool
```

How you set this up depends on whether `ydotoold` runs as **root** (a system
service — Fedora/Debian/Ubuntu) or as **your user** (an Arch user unit).

#### System service running as root (Fedora, Debian/Ubuntu)

This is the common case. `ydotoold` runs as root, so it already has `/dev/uinput`
access — **you do not need a udev rule or the `input` group.** Those only matter
when the daemon runs as your user (next section).

The real problem is the **socket**. Run as root, `ydotoold` creates
`/tmp/.ydotool_socket` owned `root:root 0600`, which (a) your unprivileged
clients can't open, and (b) isn't where the client looks by default
(`$XDG_RUNTIME_DIR/.ydotool_socket`, i.e. `/run/user/<uid>/.ydotool_socket`).
Two mismatches, both silent.

Fix both with a drop-in that pins a known path, hands ownership to your user,
and makes it group-readable (replace `1000:1000` with your `id -u`:`id -g`):

```bash
sudo systemctl enable ydotool          # Fedora unit name; Debian: ydotoold
sudo mkdir -p /etc/systemd/system/ydotool.service.d
sudo tee /etc/systemd/system/ydotool.service.d/socket.conf >/dev/null <<'EOF'
[Service]
ExecStart=
ExecStart=/usr/bin/ydotoold --socket-path=/tmp/.ydotool_socket --socket-perm=0660 --socket-own=1000:1000
EOF
sudo systemctl daemon-reload
sudo systemctl restart ydotool
```

Then tell clients where the socket is. qol already does this internally
(`inject.rs` sets `YDOTOOL_SOCKET=/tmp/.ydotool_socket` unless you override it),
so qol works with no further config. For your own shell, make it permanent:

```bash
echo 'YDOTOOL_SOCKET=/tmp/.ydotool_socket' | sudo tee -a /etc/environment
```

Because the socket is owned by your user (not `root:input`), this works
**without** the `input` group and **without** a logout — group membership added
by `usermod -aG` doesn't reach an already-running GNOME session anyway, which is
the usual reason "it worked after I logged out" turns out false.

#### Daemon running as your user (Arch AUR user unit)

Here `ydotoold` runs as you, so it needs `/dev/uinput` access via a udev rule and
the `input` group, and the socket lands in `$XDG_RUNTIME_DIR` where the client
already looks — no `YDOTOOL_SOCKET` needed:

```bash
echo 'KERNEL=="uinput", MODE="0660", GROUP="input"' | \
  sudo tee /etc/udev/rules.d/80-uinput.rules
sudo udevadm control --reload && sudo udevadm trigger
sudo usermod -aG input "$USER"          # then fully log out + back in (or reboot)
systemctl --user enable --now ydotool
```

#### Verify

```bash
systemctl status ydotool --no-pager                  # active (running)
ls -l "${YDOTOOL_SOCKET:-/tmp/.ydotool_socket}"      # socket exists, owned by you
YDOTOOL_SOCKET=/tmp/.ydotool_socket ydotool type "hello"   # types into focused window
```

- `failed to connect socket … No such file or directory` → daemon isn't running,
  or the client is looking at the wrong path (set `YDOTOOL_SOCKET`).
- `failed to connect socket … Permission denied` → the socket is owned
  `root:input` and your session isn't in `input`; use the `--socket-own` drop-in
  above instead of relying on the group.
- `failed to open uinput device` → only with a user-run daemon: the udev rule or
  `input` group hasn't taken effect (reboot to be sure).

qol picks the backend automatically at startup. Look for
`selected injection backend = Ydotool` in the logs to confirm.

### Wayland hotkey — use `qol <cmd>` + GNOME Custom Shortcut

`tauri-plugin-global-shortcut` can't grab keys under GNOME Wayland (Mutter
refuses the X11-style key grab), and the modern `xdg-desktop-portal`
GlobalShortcuts interface rejects non-sandboxed apps because the portal
sends an empty `app_id` and `gnome-control-center` discards the request:

```
gnome-control-center-global-shortcuts-provider:
  Discarded shortcut bind request from application with an invalid app_id ><.
```

Workaround: the running app always opens a Unix socket at
`$XDG_RUNTIME_DIR/qol.sock`, and the `qol` binary doubles as a client for it:
`qol toggle` pokes the socket and exits without launching a second GUI. Bind a
GNOME Custom Shortcut to it for a working Wayland hotkey:

1. **Settings → Keyboard → View and Customize Shortcuts → Custom Shortcuts → +**
   - **Name**: `qol toggle dictation`
   - **Command**: `qol toggle` (use the full path, e.g.
     `/usr/bin/qol toggle`, if `qol` isn't on the shortcut's PATH)
   - **Shortcut**: pick your combo (e.g. `Ctrl+Alt+Space` — make sure
     nothing else has it; `Super+Space` is grabbed by GNOME's input-source
     switcher and won't reach the command)
2. Start `qol` once so the socket exists, then press your combo. First
   press starts dictation, second press stops it.

Since GNOME custom keybindings only fire on press (no release event), the
hotkey is **toggle**, not push-to-talk. Aavaaz's VAD finalizes segments
naturally during dictation; toggling again ends the session.

Other subcommands:

```bash
qol status     # prints "idle" or "recording"
qol start      # idempotent
qol stop       # idempotent
qol toggle     # start if idle, stop if recording
```

The trigger socket is enabled on every OS, so these subcommands work from
scripts on macOS and X11 too. On X11/macOS/Windows you also still have real
push-to-talk through the in-process global-shortcut plugin — pick whichever
feels better.

## Run

```bash
# in one terminal — start Aavaaz with a model that fits your GPU
cd ../Aavaaz/aavaaz
source .venv/bin/activate
aavaaz serve --model distil-large-v3

# in another — build and run qol
cd ../../qol
pnpm install
pnpm tauri dev
```

Then press your hotkey (default `Super+Space`), speak, and release.

## First run — testing against a local Aavaaz

This walks through the end-to-end smoke test from a cold start. Aimed at a
single workstation: Aavaaz running on `localhost`, qol injecting into the
focused text field.

### 1. Build qol once

```bash
cd ~/src/qol
pnpm install
( cd src-tauri && cargo build )
```

The first build pulls a lot of dependencies (~5 min). Subsequent builds are
seconds.

### 2. Start Aavaaz

Pick a model that fits your GPU's VRAM. For a 6 GB card (e.g. RTX 3060):

```bash
cd ~/src/Aavaaz/aavaaz
source .venv/bin/activate
aavaaz serve --model distil-large-v3
```

You should see something like:

```
INFO  whisper_live - WebSocket server listening on 0.0.0.0:9090
INFO  whisper_live - Loaded distil-large-v3 on cuda:0
```

Sanity-check from another terminal:

```bash
ss -tln | grep 9090            # port is listening
```

### 3. Disable polish for the first test

We want to isolate STT before mixing in an LLM. Either toggle it off in the
settings window after first launch, or pre-seed the config:

```bash
mkdir -p ~/.config/qol
cat > ~/.config/qol/config.json <<'EOF'
{
  "aavaaz_url": "ws://localhost:9090",
  "model": "distil-large-v3",
  "language": "en",
  "hotkey": "Super+Space",
  "polish": {
    "enabled": false,
    "base_url": "https://api.openai.com/v1",
    "model": "gpt-4o-mini",
    "api_key_env": "OPENAI_API_KEY",
    "per_app_tone": true
  },
  "hotwords": [],
  "inject_method": "type"
}
EOF
```

### 4. Run qol with logs visible

```bash
RUST_LOG=qol=debug,warn ~/src/qol/src-tauri/target/debug/qol
```

You should see roughly:

```
INFO qol::inject: selected injection backend backend=Enigo
```

(`backend=Ydotool` if you're on Wayland with `ydotool` installed.)

The window stays hidden. Look for the tray icon — see
[Troubleshooting](#troubleshooting) if it's missing on GNOME.

### 5. Dictate

1. Focus any text field (a terminal, gedit, your browser address bar).
2. Hold `Super+Space`, say a sentence, release.
3. Watch the qol logs — you should see:
   ```
   INFO qol: session started
   DEBUG qol::session: session started app=Some("...")
   INFO qol: session stopped
   ```
4. The transcript should land in the focused field within ~1 second of
   release.

### 6. Enable polish (optional)

Open the settings window from the tray, check **Clean up transcripts**, and
configure:

- **OpenAI**: `https://api.openai.com/v1`, model `gpt-4o-mini`
- **Groq** (very fast): `https://api.groq.com/openai/v1`, model `llama-3.1-8b-instant`
- **Local Ollama**: `http://localhost:11434/v1`, model `qwen2.5:7b-instruct`, no key

Paste the key into **API key** and hit **Save key to keyring** — it's stored in
the OS keyring (GNOME Keyring/KWallet, macOS Keychain, Windows Credential
Manager), keyed by `base_url`, and never written to `config.json` or shown back.
Blank the field and save to clear it. If no key is stored, qol falls back to the
env var named in **API key env var** (`export` it in the shell you launch qol
from, then restart). Local servers need no key at all.

### Troubleshooting

| Symptom | Likely cause | Fix |
|---|---|---|
| Aavaaz errors `libcudnn_ops_infer.so: cannot open` | cuDNN not on path | `pip install nvidia-cudnn-cu12` inside the Aavaaz venv |
| Aavaaz `CUDA out of memory` | Model too big for your VRAM | Use `distil-large-v3` or `medium` instead of `large-v3` |
| qol logs "no default input device" | Mic not picked by PulseAudio/Pipewire | `pactl list sources short`; set a default with `pactl set-default-source <name>` |
| No tray icon on GNOME | GNOME hides AppIndicators by default | `sudo dnf install gnome-shell-extension-appindicator`, then enable "AppIndicator and KStatusNotifierItem Support" |
| Hotkey does nothing | Already grabbed by another app | Pick a different combo in settings (e.g. `Ctrl+Alt+Space`) |
| Text doesn't appear in focused app (GNOME Wayland) | Wayland blocks synthetic input | Install `ydotool` + `ydotoold` (see Wayland section above); restart qol; verify `backend=Ydotool` in logs |
| Polish silently produces no text | No key in keyring and env var unset/wrong | Save the key in settings, or `echo $OPENAI_API_KEY` and restart qol after `export`-ing |
| `connect failed: ConnectionRefused` | Aavaaz not running | Start it on `:9090` first |

### What "good" looks like

End-to-end, on a 6 GB GPU with polish disabled, expect roughly:

- Hotkey press → first PCM frame to Aavaaz: <50 ms
- End of speech → first completed segment from Aavaaz: 300–800 ms (depends on VAD pause threshold)
- First completed segment → text in focused app: <50 ms
- With polish enabled (OpenAI `gpt-4o-mini`): add ~300–600 ms per segment

If you're seeing multi-second lag, that's almost always Aavaaz model load
or CPU fallback (check `nvidia-smi` while dictating — qol should drive the
GPU to ~30% utilization momentarily).

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
    "per_app_tone": true,
    "tone_profiles": [
      { "apps": ["slack", "discord", "telegram"], "tone": "casual chat" },
      { "apps": ["mail", "thunderbird", "outlook", "gmail"], "tone": "professional email" },
      { "apps": ["code", "vscode", "zed", "nvim"], "tone": "terse, code-friendly" }
    ],
    "default_tone": "natural prose"
  },
  "hotwords": ["Aavaaz", "qol", "WhisperLive"],
  "inject_method": "type"
}
```

Each `tone_profiles` rule matches when any of its `apps` tokens is a
case-insensitive substring of the focused app name; the first matching rule
wins, else `default_tone` applies. With `per_app_tone` off, `default_tone` is
always used. Edit rules in the settings window or here directly.

The polish API key is never stored in `config.json`. At request time qol reads
the OS keyring first (service `qol`, account = `base_url`), then falls back to
the env var named by `api_key_env`. Manage the stored key from the settings
window (**Save key to keyring** / blank + save to clear); the UI only shows
whether a key exists, never its value.

## Tests & CI

```bash
cd src-tauri
cargo test          # unit tests (config round-trip, hotkey parser)
cargo clippy --all-targets -- -D warnings
cargo fmt -- --check
```

CI runs the above on **Ubuntu, macOS, and Windows** for every push and PR — see
[.github/workflows/ci.yml](.github/workflows/ci.yml).

The network-bound paths are covered against in-process fakes: the Aavaaz
WebSocket session (`transport.rs`) and the LLM polish pass (`polish.rs`,
including failure/timeout/empty-response fallbacks). Hardware-bound paths
(audio capture, keystroke injection) aren't tested yet; that needs a virtual
audio device and a headless injection backend.

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
- [x] Tray recording indicator (icon + tooltip change while capturing)
- [x] Desktop notifications on start failure / backend drop
- [x] Settings UI
- [x] Tone-rolling-context across segments (consistency in long dictation)
- [x] Local-only polish via llama.cpp / Ollama (OpenAI-compatible `base_url`)
- [x] Integration tests with a fake Aavaaz WS server + stub polish endpoint
- [x] Per-app tone profiles configurable in UI
- [x] API key stored in OS keyring (keyring-first, env var fallback)
- [x] Start-at-login toggle (`tauri-plugin-autostart`)
- [x] Release workflow: draft GitHub release with per-OS bundles on a `v*` tag
- [x] Wayland trigger folded into the `qol` binary (`qol toggle`), so packaged bundles carry it

## License

MPL-2.0
