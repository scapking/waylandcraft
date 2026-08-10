# WaylandCraft 🎮🪟

**Run Linux desktop apps inside Minecraft** — A Fabric mod that integrates a Wayland compositor into Minecraft, allowing players to view and interact with Linux desktop windows in-game. Supports multi-player window sharing.

> ⚠️ This project is based on the original [WaylandCraft](https://github.com/evvie-jpg/waylandcraft). Multi-player display and other features were AI-implemented. **Functionality and security are NOT guaranteed.** Use at your own risk.

<p align="center">
  <img src="https://img.shields.io/badge/Minecraft-26.1.2-green" />
  <img src="https://img.shields.io/badge/Fabric%20Loader-0.19.2+-blue" />
  <img src="https://img.shields.io/badge/Fabric%20API-0.147.0%2B-blue" />
  <img src="https://img.shields.io/badge/Java-25-orange" />
  <img src="https://img.shields.io/badge/Version-v0.2.32-brightgreen" />
</p>

---

## Download

👉 **[Latest Release (v0.2.32)](https://github.com/scapking/waylandcraft/releases/latest)** — Download `waylandcraft.jar` and drop it into your `mods/` folder.

> The upstream repository (almightydb) Releases page lags behind; grab the latest build from the link above.

---

## Highlights (v0.2.32)

- **Server-side multi-thread frame relay** — frames are sharded across N threads by window handle (same window keeps order, different windows relay in parallel); register/unregister run on the server thread, so the netty thread is never blocked by list broadcasts.
- **Reliable JPEG degrade** — windows with transparent pixels no longer get stuck in PNG (lossless, quality had no effect); oversize shared frames are force-encoded to JPEG with alpha blended on black, so quality degradation actually works.
- **Higher frame limit** — single-frame JPEG/PNG limit raised from 600 KB to 1.8 MB (aligned with the server protocol cap), oversize frames are no longer dropped for ordinary high-res windows.

---

## Features

| Feature | Description |
|---------|-------------|
| Pure CLI mode | Sci-fi UI removed; vanilla rendering restored; everything is driven by `/wl` commands |
| Windows in the world | Display Wayland windows in the game world; drag, resize, pin, hide |
| Unified rendering | Local and remotely shared windows share the same render path for identical visuals |
| Multi-player sharing | Share windows to other players, rendered live in their world |
| Desktop capture | XDG Desktop Portal + PipeWire window capture |
| Permissions | 4 levels: NONE / VIEW / INTERACT / CONTROL |
| Iris (shaders) compatible | Falls back to vanilla pipeline automatically when Iris is loaded; windows display correctly with shaders on |
| Adaptive quality | Configurable scale, JPEG quality, framerate, bitrate; built-in presets |
| Performance | PBO async readback, GPU scaling, diff-frame transfer, heartbeat frames, auto PNG/JPEG, server multi-thread frame relay (off main thread) |

---

## Demo Screenshots

<p align="center">
  <img src="assets/demo_1.jpg" width="49%" alt="Demo 1" />
  <img src="assets/demo_2.jpg" width="49%" alt="Demo 2" />
</p>

> The images above are demo examples only.

---

## Installation

### Requirements

- Minecraft **26.1.2** (Java Edition)
- Fabric Loader **0.19.x** + Fabric API **0.147.0+26.1.2** (or matching version)
- **Java 25**
- Linux + **Wayland** session (the native library handles window capture; X11 is not supported)
- **xwayland-satellite is bundled** — X11-only apps get a `DISPLAY` automatically, no manual install needed (still requires a system `Xwayland`, which is present on virtually all Wayland desktops). Both **x86_64 and arm64** binaries are shipped inside the jar, so ARM64 hosts (e.g. Raspberry Pi) work out of the box too.

### Steps

1. Install Fabric Loader and Fabric API
2. Put `waylandcraft.jar` into `.minecraft/mods/`
3. **Multi-player: the mod must also be installed on the server** (`give`, `permission`, `share` logic is registered server-side; without it these features silently fail — e.g. `/wl give` does nothing)
4. Launch the game (a single-player world is a built-in server; client and server share the same `mods/` folder)
5. **Android (viewer-only)**: install `waylandcraft-android-<arch>.jar` (local features auto-disable when no native Wayland is available — it never crashes). Join any server running this mod and you'll see windows shared by desktop teammates (`/wl share start <handle>` is initiated by the desktop player; the phone needs no extra steps)

---

## Quick Start

1. Launch an app: `/wl launch firefox`
2. List windows: `/wl list windows`
3. Turn a window into an item: `/wl give <handle>` → **hold right-click** on the item to place it in the world
4. Capture the keyboard to operate the window: `/wl grab <handle>` (or press `G` to toggle)
5. Share with teammates: `/wl share start <handle>`

### Keybinds

| Key | Function |
|-----|----------|
| `B` | Open the window manager screen |
| `G` | Toggle keyboard capture / release (while captured, keys pass through to the window) |
| `Right-click hold` + WindowItem | Display window in the world |

> All other operations go through `/wl` commands — full list below.

---

## Commands

`<handle>` accepts four formats: `0x` short handle / full handle / **instance alias `wN` (shown by `/wl list windows`, unique per session)** / app alias (e.g. `firefox_esr`). For duplicate windows use `alias:N` (e.g. `firefox:2`).

### Window Management

| Command | Function |
|---------|----------|
| `/wl list` | List launchable apps (default) |
| `/wl list windows` | List windows in the compositor |
| `/wl list apps` | List launchable apps |
| `/wl list desktop` | List capturable desktop windows |
| `/wl launch <app>` | Launch an app (by name or exact alias; use the full alias to disambiguate same-prefix apps, e.g. `visual_studio_code`) |
| `/wl capture` | Open the Portal picker to capture a desktop window |
| `/wl give <handle>` | Turn a window into an item in your inventory |
| `/wl take <handle>` | Take the window item back |
| `/wl grab <handle>` | Grab the window (drag with mouse; scroll to move forward/backward) |
| `/wl show <handle>` | Show the window in the world |
| `/wl hide <handle>` | Hide the window from the world |
| `/wl pin <handle>` | Pin the window (stays displayed, unaffected by hide/minimize) |
| `/wl unpin <handle>` | Unpin the window |
| `/wl close <handle>` | Terminate the app (close the window) |
| `/wl resize <handle> <w> <h>` | Resize the window |
| `/wl pos <handle>` | Show window position (x/y/z), facing angle, scale, resolution |
| `/wl move <handle> <x> <y> <z>` | Set window position (absolute like `100.5`, or relative offset like `~0.5` / `~-1` / `~`) |
| `/wl rotate <handle> <angle>` | Set window facing angle (degrees, around Y; absolute like `90`, or relative like `~15`; `0`=facing +Z, `90`=facing +X) |
| `/wl template save <name>` | Save layout of all windows in current chunk as a temporary template (lost on restart) |
| `/wl template savep <name>` | Save a permanent template (app + position + resolution, written to disk) |
| `/wl template apply <name>` | Apply a temporary template, restoring window positions |
| `/wl template applyp <name>` | Apply a permanent template: auto-launch the app and place it per record |
| `/wl template list` | List all templates |
| `/wl template remove <name>` / `removep <name>` | Remove a temporary / permanent template |

### Share Management

| Command | Function |
|---------|----------|
| `/wl share start <handle>` | Start sharing the window |
| `/wl share stop <handle>` | Stop sharing |
| `/wl share quality <handle> <s> <q> <fps>` | Set quality (scale, quality, fps) |
| `/wl share preset <handle> <preset>` | Apply a preset (below) |
| `/wl share config <handle> <param> <value>` | Set a single parameter |
| `/wl share reset <handle>` | Reset quality to defaults |
| `/wl share info <handle>` | Show current share config |
| `/wl share resolution <handle> <w> <h>` | Set target resolution |
| `/wl share stats <handle>` | Show sharing statistics |

### Permission Management

| Command | Function |
|---------|----------|
| `/wl permission list` | List all permissions |
| `/wl permission default <PERM>` | Set the default permission |
| `/wl permission allow <player> <PERM>` | Add to whitelist |
| `/wl permission deny <player>` | Add to blacklist |
| `/wl permission remove <player>` | Remove a player |

> `PERM`: `NONE` / `VIEW` / `INTERACT` / `CONTROL`

### Settings

| Command | Function |
|---------|----------|
| `/wl settings list` | List current settings |
| `/wl settings set <key> <value>` | Change a setting |

| Param | Default | Description |
|-------|---------|-------------|
| `pixelsPerBlock` | `500` | Window pixel density per block in the world |
| `windowAntialiasing` | `false` | Window RGSS antialiasing (custom pipeline only, no shaders) |
| `focusOnHover` | `false` | Auto-focus window on mouse hover |

### Share Parameters & Presets

| Param | Description | Range |
|-------|-------------|-------|
| `scale` | Resolution scale | 0.1 – 1.0 |
| `quality` | JPEG quality | 0.1 – 1.0 |
| `fps` | Max framerate | 5 – 120 |
| `bitrate` | Max bitrate (kbps) | 0 = unlimited |
| `diffThreshold` | Pixel change threshold | 0.001 – 1.0 |

| Preset | Scale | Quality | FPS | Bitrate |
|--------|-------|---------|-----|---------|
| `performance` | 0.25 | 0.5 | 60 | 1000 kbps |
| `balanced` | 0.5 | 0.7 | 30 | 2000 kbps |
| `quality` | 1.0 | 1.0 | 30 | unlimited |
| `lowlatency` | 0.35 | 0.6 | 60 | 1500 kbps |

---

## Iris (Shaders) Compatibility

- Windows display correctly with Iris shaders: when Iris is loaded the mod automatically falls back to the **vanilla entity pipeline**, and window content stays full-brightness (unaffected by shader lighting)
- Without shaders, the custom pipeline is used and RGSS antialiasing is available (`windowAntialiasing`)
- Both modes render the window **textured on the front, solid black on the back** — identical behavior

---

## Notes

- **Windows are always vertical**: windows are placed upright (cannot be tilted), the vertical axis (y) is locked while dragging (horizontal movement only), and the window bottom stays at least **0.4 blocks** above the ground at that spot; `Ctrl+Scroll` rotates the facing (staying vertical)
- **Precise placement**: check the current position/angle with `/wl pos <handle>`, then set exact coordinates with `/wl move <handle> <x> <y> <z>` (supports `~` relative offsets) and exact facing with `/wl rotate <handle> <angle>` (degrees)
- **The server must have the mod installed**: in multiplayer, server-side features (`give` / `permission` / `share`) depend on it, otherwise requests are silently dropped
- Slight aliasing at rounded corners/shadows is normal for JPEG; windows with transparency automatically switch to PNG to preserve alpha (when a shared frame exceeds the size limit, it is force-encoded to JPEG: alpha is blended onto a black background, only quality drops, UI size never changes)
- Desktop capture (`/wl capture`) requires the system XDG Desktop Portal and a Wayland session
- Full in-game help: `/wl help`

---

## Known Limitations

The current version still has a few rough edges that will be improved in future releases:

1. **Window movement is deliberately constrained** — windows are fixed vertical with the height axis locked while dragging (bottom stays ≥ 0.4 blocks above ground); this is an intentional simplification, and freer placement may be added later.
2. **Quality vs latency tradeoff in sharing** — to keep the UI size identical to the sharer, only JPEG quality is lowered (never resolution) when frames exceed the limit; high-resolution windows still put pressure on weak servers/phones during relay and decode.

---

## Build

```bash
# Prerequisites: Java 25, Rust toolchain, Wayland dev libraries
apt install libwayland-dev libxkbcommon-dev pkg-config libclang-dev

# 1. Build the Rust native library (must use release; build.gradle prefers release .so)
source ~/.cargo/env
cd native && cargo build --release

# 2. Build the Java mod
cd .. && ./gradlew clean build

# Output: build/libs/waylandcraft.jar (~6.0MB, includes both x86_64 and arm64 xwayland-satellite)
```

> ⚠️ Make sure the packaged native library is `native/target/release/libwaylandcraft.so` (~3.7MB). Accidentally packaging a debug build (176MB with debug symbols) inflates the jar to 39MB.

> 📦 The bundled `xwayland-satellite` binaries are built by `native/build-satellite.sh` — run it once for `x86_64` and once for `arm64` (cross-compile with `aarch64-linux-gnu-gcc` + `cargo build --target aarch64-unknown-linux-gnu`), then place both under `native/`.

---

## Tech Stack

| Layer | Technology |
|-------|------------|
| Game | Java 25, Fabric Loader 0.19.2+, Fabric API 0.147.0+ |
| Native Bridge | Rust, JNI |
| Wayland | Smithay, wayland-client, wlr-foreign-toplevel-management |
| Image | PBO async readback, glBlitFramebuffer, JPEG/PNG, MemoryUtil |
| Network | Fabric Networking API, custom Payload protocol |

---

## License

MIT License

## Acknowledgments

- [WaylandCraft](https://github.com/evvie-jpg/waylandcraft) — Original project
- [Smithay](https://github.com/Smithay/smithay) — Wayland compositor framework
- [Fabric](https://fabricmc.net/) — Minecraft mod loader
