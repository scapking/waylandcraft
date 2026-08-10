<p align="center">
  <h1 align="center">🎮🪟 WaylandCraft</h1>
  <p align="center"><b>Run real Linux desktop applications inside Minecraft.</b></p>
  <p align="center">
    <a href="README.md">English</a> · <a href="README_ZH.md">中文</a> · <a href="README_JA.md">日本語</a>
  </p>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/Minecraft-26.1.2-green" />
  <img src="https://img.shields.io/badge/Fabric%20Loader-0.19.2+-blue" />
  <img src="https://img.shields.io/badge/Fabric%20API-0.147.0%2B-blue" />
  <img src="https://img.shields.io/badge/Java-25-orange" />
  <img src="https://img.shields.io/badge/Platform-Linux%20%28capture%2Bshare%29-lightgrey" />
  <img src="https://img.shields.io/badge/Platform-Win%2FmacOS%2FAndroid%20%28viewer%29-lightgrey" />
  <img src="https://img.shields.io/badge/Version-v0.2.35-brightgreen" />
  <img src="https://img.shields.io/badge/License-MIT-blue" />
</p>


> ⚠️ **Disclaimer** — This project is based on the original [WaylandCraft](https://github.com/EVV1E/waylandcraft.git). Multi-player display and other features were AI-implemented. **Functionality and security are NOT guaranteed.** Use at your own risk.

---

## Table of Contents

- [✨ Features](#-features)
- [🗺️ Platform Support](#️-platform-support)
- [🚀 Quick Start](#-quick-start)
- [📖 Usage Guide](#-usage-guide)
- [📚 Command Reference](#-command-reference)
- [⚙️ Configuration](#️-configuration)
- [🎨 Shader Compatibility](#-shader-compatibility)
- [💡 Tips & Best Practices](#-tips--best-practices)
- [❓ FAQ](#-faq)
- [🚧 Known Limitations](#-known-limitations)
- [🏗️ Build from Source](#️-build-from-source)
- [🧱 Architecture](#-architecture)
- [🤝 Contributing](#-contributing)
- [📜 Changelog](#-changelog)
- [📄 License](#-license)

---

## ✨ Features

### 🖥️ Real Linux Apps in Your World

Turn any Wayland window into an in-game object. Launch apps, view them on a virtual screen inside the world, and interact with them — all without leaving Minecraft.

- **Pure CLI mode** — all operations via `/wl` commands; vanilla rendering, no sci-fi UI clutter
- **Unified rendering** — local and remotely shared windows share the same render path for identical visuals
- **Window placement** — drag, resize, pin, hide, rotate; save/restore layouts with templates
- **X11 app support** — bundled `xwayland-satellite` gives X11 applications a `DISPLAY` automatically (both `x86_64` and `arm64` binaries included)

### 👥 Multiplayer Window Sharing

Stream your desktop to other players in real time. Built as a first-class server-side feature, not an afterthought.

- **Live sharing** — other players see your windows rendered in their world
- **Mobile viewing** — Android clients can view shared windows with zero extra setup
- **Server-side relay** — frames are forwarded off the main thread; multi-window sharing is sharded across worker threads, so one slow viewer never blocks another window
- **Adaptive quality** — configurable scale, JPEG quality, framerate, bitrate, plus built-in presets

### 🔐 Granular Permissions

Four-level permission model per player per window:

| Level | Meaning |
|-------|---------|
| `NONE` | Window is invisible and unknown to the player |
| `VIEW` | Can see the shared window in the world |
| `INTERACT` | Can send mouse/keyboard events to the window |
| `CONTROL` | Can resize, reposition and manage permissions |

Whitelist, blacklist, and per-window overrides are all supported.

### ⚡ Performance Engineering

- PBO async readback + GPU scaling (`glBlitFramebuffer`)
- Diff-frame transfer: only changed regions are encoded
- Heartbeat frames for idle windows; `PNG`/`JPEG` auto-switch for transparency
- Oversized frames are degraded by JPEG quality only — **UI size never changes** (when a window has transparent pixels, oversize frames are force-encoded to JPEG with alpha blended onto black, so degradation actually works)

### 🎨 Shader Compatible

Detects Iris and falls back to the vanilla entity pipeline automatically — windows display correctly even with shaders enabled, always at full brightness.

---

### 🗺️ Platform Support

| Platform | Capture local windows | View shared windows |
|----------|:---:|:---:|
| Linux x86_64 / arm64 | ✅ | ✅ |
| Android (viewer) | ❌ | ✅ |
| Windows (viewer) | ❌ | ✅ |
| macOS (viewer) | ❌ | ✅ |
| iOS (viewer, experimental) | ❌ | ✅ |

- **Full mode (Linux)** — capture, share and view.
- **Viewer-only mode (Android / Windows / macOS)** — install the same `waylandcraft.jar`; the mod auto-detects the platform, disables local capture, and keeps receiving shared windows. No separate build needed.
- **iOS (viewer, experimental)** — run Minecraft Java Edition + Fabric via [PojavLauncher](https://github.com/PojavLauncherTeam/PojavLauncher_iOS) (or its successor [Amethyst](https://github.com/AngelAuraMC/Amethyst-iOS)), then install the same `waylandcraft.jar` in viewer-only mode. Not yet field-tested.

---

## 🚀 Quick Start

### Prerequisites

- Minecraft **26.1.2** (Java Edition)
- Fabric Loader **0.19.x** + Fabric API **0.147.0+26.1.2**
- **Java 25**
- Linux with a **Wayland** session for full mode (capture requires Wayland; X11-only sessions are not supported) — Windows/macOS/Android run in viewer-only mode, see [Platform Support](#️-platform-support)

### Installation

1. Install Fabric Loader and Fabric API.
2. Drop `waylandcraft.jar` into `.minecraft/mods/`.
3. **Multi-player: the server must run the mod too** (give/permission/sharing logic lives server-side; without it these features silently no-op).
4. Launch the game — single-player worlds embed a server and share the same `mods/` folder.

### First Steps

```text
/wl launch firefox              # Launch an app (or press V)
/wl list windows                # List windows; 4-char random aliases shown at line end
/wl give <handle>               # Turn the window into an item; right-hold to place in the world
/wl grab <handle>               # Grab the window and drag it around (G toggles keyboard capture)
/wl share start <handle>        # Share it with your teammates
```

> 💡 **Android viewer?** Install `waylandcraft-android-<arch>.jar` and join the server — shared windows appear automatically, no configuration needed.

---

## 📖 Usage Guide

### Window Management

| Action | Command |
|--------|---------|
| List launchable apps | `/wl list` · `/wl list apps` |
| List windows | `/wl list windows` |
| Capture a desktop window | `/wl capture` |
| Launch an app | `/wl launch <app>` |
| Turn window into item | `/wl give <handle>` |
| Take window back | `/wl take <handle>` |
| Grab / drag window | `/wl grab <handle>` |
| Show / hide in world | `/wl show <handle>` / `/wl hide <handle>` |
| Pin (always visible) | `/wl pin <handle>` / `/wl unpin <handle>` |
| Terminate app | `/wl close <handle>` |
| Resize window | `/wl resize <handle> <w> <h>` |
| Position / inspect | `/wl pos <handle>` |
| Move (abs or `~` relative) | `/wl move <handle> <x> <y> <z>` |
| Rotate (degrees) | `/wl rotate <handle> <angle>` |

**Handle formats** — `<handle>` accepts: `0x` short handle, full handle, **instance alias** (4-char random, e.g. `k7xq`, from `/wl list windows`, unique per session), or app alias (e.g. `firefox_esr`). Multiple windows of the same app: `alias:N` (e.g. `firefox:2`).

### Layout Templates

| Command | Purpose |
|---------|---------|
| `/wl template save <name>` | Save current window layout (temporary, lost on restart) |
| `/wl template savep <name>` | Save permanent template (app + position + resolution, on disk) |
| `/wl template apply <name>` | Restore temporary template |
| `/wl template applyp <name>` | Apply permanent template: auto-launch apps and place them |
| `/wl template list` | List all templates |
| `/wl template remove <name>` / `removep <name>` | Delete temporary / permanent template |

### Sharing

| Command | Purpose |
|---------|---------|
| `/wl share start <handle>` | Start sharing a window |
| `/wl share stop <handle>` | Stop sharing |
| `/wl share quality <handle> <s> <q> <fps>` | Set scale / quality / framerate |
| `/wl share preset <handle> <preset>` | Apply a preset (see [Configuration](#️-configuration)) |
| `/wl share config <handle> <param> <value>` | Tune a single parameter |
| `/wl share reset <handle>` | Reset to defaults |
| `/wl share info <handle>` | Show current share config |
| `/wl share resolution <handle> <w> <h>` | Set target resolution |
| `/wl share stats <handle>` | Show share statistics |

### Permissions

| Command | Purpose |
|---------|---------|
| `/wl permission list` | List all permissions |
| `/wl permission default <PERM>` | Set default permission |
| `/wl permission allow <player> <PERM>` | Whitelist a player |
| `/wl permission deny <player>` | Blacklist a player |
| `/wl permission remove <player>` | Remove a player |

`PERM`: `NONE` / `VIEW` / `INTERACT` / `CONTROL`

---

## 📚 Command Reference

### Settings

| Command | Purpose |
|---------|---------|
| `/wl settings list` | Show current settings |
| `/wl settings set <key> <value>` | Change a setting |

### Share Quality Parameters

| Parameter | Description | Range |
|-----------|-------------|-------|
| `scale` | Resolution scale | 0.1 – 1.0 |
| `quality` | JPEG quality | 0.1 – 1.0 |
| `fps` | Max framerate | 5 – 120 |
| `bitrate` | Max bitrate (kbps) | 0 = unlimited |
| `diffThreshold` | Pixel-change threshold | 0.001 – 1.0 |

### Presets

| Preset | Scale | Quality | FPS | Bitrate |
|--------|-------|---------|-----|---------|
| `performance` | 0.25 | 0.5 | 60 | 1000 kbps |
| `balanced` | 0.5 | 0.7 | 30 | 2000 kbps |
| `quality` | 1.0 | 1.0 | 30 | unlimited |
| `lowlatency` | 0.35 | 0.6 | 60 | 1500 kbps |

### Global Settings

| Key | Default | Description |
|-----|---------|-------------|
| `pixelsPerBlock` | `500` | Pixel density of a window per block |
| `windowAntialiasing` | `false` | RGSS antialiasing (no shaders only) |
| `focusOnHover` | `false` | Auto-focus window on hover |

---

## 🎨 Shader Compatibility

- With Iris shaders enabled, windows render through the **vanilla entity pipeline** at full brightness — unaffected by shader lighting.
- Without shaders, a custom pipeline is used with optional RGSS antialiasing (`windowAntialiasing`).
- Both modes render the window front face textured, **back face solid black** — identical behavior.

---

## 💡 Tips & Best Practices

- **Windows stay vertical** — windows are always upright; the vertical axis is locked while dragging and the bottom edge never goes below **0.4 blocks** above the floor. `Ctrl+Scroll` rotates the facing (still vertical).
- **Precise placement** — read the current pose with `/wl pos <handle>`, then set exact coordinates with `/wl move` (`~` relative offsets supported) and facing with `/wl rotate`.
- **Server must run the mod** — in multiplayer, `give` / `permission` / `share` depend on server-side logic; without the mod on the server, requests are silently dropped.
- **Slight aliasing** at rounded corners/shadows is normal for JPEG; transparent windows automatically use PNG to preserve alpha.
- **Full help in-game**: `/wl help`.

---

## ❓ FAQ

**Q: Why must the server also install the mod?**
A: `give`, permission, and sharing logic is registered server-side. Without the mod on the server these features silently no-op.

**Q: Can I view shared windows on my phone?**
A: Yes. Install `waylandcraft-android-<arch>.jar` and join the server. Windows shared by PC teammates appear automatically — no extra steps on the phone.

**Q: Do X11 applications work?**
A: Yes. `xwayland-satellite` is bundled inside the jar (both `x86_64` and `arm64`), so X11 apps get a `DISPLAY` automatically. The system still needs `Xwayland`, which ships with virtually every Wayland desktop.

**Q: Does it work on Windows/macOS?**
A: Viewer-only. Install the same `waylandcraft.jar` — the mod auto-detects the platform, disables local window capture, and still receives shared windows. On iOS, use PojavLauncher/Amethyst to run Java Edition + Fabric (experimental, not yet field-tested).

**Q: Shared image is blurry / laggy. How do I improve it?**
A: Raise quality or fps: `/wl share quality <handle> <scale> <quality> <fps>`, or apply the `quality` preset. Remember the default is a balanced profile; UI size is preserved even when quality degrades.

**Q: The window looks transparent/black with shaders on.**
A: Iris is auto-detected and the mod falls back to the vanilla pipeline. Make sure all clients and the server run the same (≥ v0.2.32) version.

**Q: How is this different from upstream WaylandCraft?**
A: This fork adds multi-player window sharing, the permission system, pure CLI mode, mobile viewing, and the performance/quality engineering described above — features implemented (AI-assisted) on top of the original project.

---

## 🚧 Known Limitations

1. **Constrained window movement** — windows are locked to the vertical axis and the bottom edge stays ≥ 0.4 blocks above the floor. This is a deliberate simplification; freer placement may come later.
2. **Quality vs latency tradeoff in sharing** — to keep the UI size identical to the sharer, only JPEG quality is lowered (never resolution) when frames exceed the limit; high-resolution windows still put pressure on weak servers/phones during relay and decode.

---

## 🏗️ Build from Source

```bash
# Prerequisites: Java 25, Rust toolchain, Wayland dev libraries
apt install libwayland-dev libxkbcommon-dev pkg-config libclang-dev

# 1. Build the Rust native library (must use release; build.gradle prefers release .so)
source ~/.cargo/env
cd native && cargo build --release

# 2. Build the Java mod
cd .. && ./gradlew clean build

# Output: build/libs/waylandcraft.jar (~6.0MB, includes x86_64 and arm64 xwayland-satellite)
```

> ⚠️ Make sure the packaged native library is `native/target/release/libwaylandcraft.so` (~3.7MB). Accidentally packaging a debug build (176MB with debug symbols) inflates the jar to 39MB.

> 📦 The bundled `xwayland-satellite` binaries are built by `native/build-satellite.sh` — run once for `x86_64` and once for `arm64` (cross-compile with `aarch64-linux-gnu-gcc` + `cargo build --target aarch64-unknown-linux-gnu`), then place both under `native/`.

---

## 🧱 Architecture

```text
┌─────────────────────────────────────────────────────────────┐
│                      Minecraft Client                       │
│  ┌──────────────┐   ┌───────────────────┐   ┌────────────┐  │
│  │  Window View │   │  WindowShare      │   │  /wl CLI   │  │
│  │  (renderer)  │◄─►│  (capture/send)   │◄─►│  (commands)│  │
│  └──────┬───────┘   └────────┬──────────┘   └────────────┘  │
│         │  PBO/GPU readback  │ Fabric Networking API        │
└─────────┼────────────────────┼──────────────────────────────┘
          │                    │
┌─────────┼────────────────────┼──────────────────────────────┐
│         ▼                    ▼            Minecraft Server  │
│  ┌─────────────────────────────────────────────┐            │
│  │  SharedWindowManager (permissions, state)   │            │
│  └──────────────────────┬──────────────────────┘            │
│                         │ frames                             │
│  ┌──────────────────────▼──────────────────────┐            │
│  │  SharedWindowFrameRelay (worker threads)    │            │
│  └──────────────────────┬──────────────────────┘            │
└─────────────────────────┼────────────────────────────────────┘
                          │ broadcast
          ┌───────────────▼────────────────┐
          │  Viewer (PC or Android client) │
          │  async decode → world render   │
          └────────────────────────────────┘
```

| Layer | Technology |
|-------|------------|
| Game | Java 25, Fabric Loader 0.19.2+, Fabric API 0.147.0+ |
| Native Bridge | Rust, JNI |
| Wayland | Smithay, wayland-client, wlr-foreign-toplevel-management |
| Image | PBO async readback, glBlitFramebuffer, JPEG/PNG, MemoryUtil |
| Network | Fabric Networking API, custom Payload protocol |

---

## 🤝 Contributing

Contributions are welcome! This is a fork of [WaylandCraft](https://github.com/EVV1E/waylandcraft.git) with AI-implemented multi-player features — expect rough edges.

- Report bugs and feature requests via [GitHub Issues](../../issues)
- Submit fixes and improvements via [Pull Requests](../../pulls)
- Keep both Java and Rust sides building: run `cargo build --release` in `native/` and `./gradlew build` in the repo root
- Update the READMEs (EN/ZH/JA) when changing user-facing behavior

---

## 📜 Changelog

See the [Releases](https://github.com/scapking/waylandcraft/releases) page for the full history.

**Recent highlights:**

- **v0.2.35** — iOS detection added (PojavLauncher/Amethyst runtime): viewer-only mode, same jar, shared windows render; platform matrix updated.
- **v0.2.34** — Windows/macOS now supported in **viewer-only mode**: platform auto-detection skips native capture; the same jar works on Linux/Windows/macOS/Android; shared windows still render.
- **v0.2.33** — Window instance aliases are now 4-char random codes (e.g. `k7xq`) instead of `w1/w2/…`; ambiguous characters `0/o/1/l/i` excluded for easier typing.
- **v0.2.32** — Force-JPEG degrade for transparent windows (quality actually works now); single-frame limit raised 600 KB → 1.8 MB.
- **v0.2.31** — Server frame relay sharded across N threads by window (same window ordered, different windows parallel); register/unregister off the netty thread.
- **v0.2.30** — Frame relay moved off the Server thread entirely; PBO permanent fallback; default q0.85 / 10 fps.

---

## 📄 License

MIT License — see [LICENSE](LICENSE) for details.

## Acknowledgments

- [WaylandCraft](https://github.com/EVV1E/waylandcraft.git) — Original project
- [Smithay](https://github.com/Smithay/smithay) — Wayland compositor framework
- [Fabric](https://fabricmc.net/) — Minecraft mod loader
