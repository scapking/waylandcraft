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
  <img src="https://img.shields.io/badge/Version-1.4.0-brightgreen" />
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
- **Full keyboard passthrough** — single keys, modifier combos (Ctrl/Shift/Alt), and long-press REPEAT all reach the focused window; capture split **G = keyboard only, J = keyboard + mouse**
- **X11 app support** — bundled `xwayland-satellite` gives X11 applications a `DISPLAY` automatically (both `x86_64` and `arm64` binaries included)

### ⌨️ Input methods — CJK that actually works, everywhere

The feature upstream called impossible, shipped end-to-end (see [docs/IME.md](docs/IME.md) for the full engineering saga):

- **Nested IM daemon** — an isolated `fcitx5` (or ibus-daemon) instance runs inside waylandcraft's own compositor with a private HOME; it auto-seeds **every engine installed on your system** (pinyin, wubi, shuangpin, cangjie, …) and mirrors your host profile when present. It never touches your desktop IME.
- **Candidate window rendered in-game** — the IM's real candidate UI is captured (ARGB frames) and drawn *at the text cursor* in Minecraft GUIs **and as a world-space quad next to in-game app windows**; mouse click selects, wheel pages. No blind typing.
- **Built-in pinyin engine, zero dependencies** — an embedded dictionary + segmentation engine engages automatically when no fcitx5 is installed, so any Linux desktop types Chinese out of the box. Switch sources with `/wl ime source auto|fcitx5|builtin`, pick engines with `/wl ime eng`.
- **Correct at the protocol level** — per-key consumed/unconsumed dispatch, no double-send, no swallowed keys; commits are guaranteed-delivered (every past incident is locked by a regression test, including real-fcitx5 live tests).
- **Visible state** — 「拼」working badge, `/wl ime info`, `ime_*` subsystem rows in `/wl status`.
- Works in **Minecraft chat, server address bar, search boxes**, and inside **shared/launched app windows** (firefox, chromium, terminals…).

### 👥 Buffered, stutter-free multiplayer

- **Latency-for-smoothness playout**: viewers buffer `sharePlayoutDelayMs` (default `auto` = load-adaptive **14–32 s**, or `0` = low-latency, or a manual ms value) on a **single shared audio+video playout clock** — smooth video under jitter, A/V phase locked, **resolution never downscaled**.
- **Per-window audio**: the shared *application's own* PipeWire stream (not your microphone), synchronized to the same playout clock, with distance falloff (`audioShareRangeBlocks`).
- **Server-side fairness**: capability-filtered relays (vanilla players receive nothing undecodable) and write backpressure (slow viewers drop frames instead of OOM-ing the server).

- **Live sharing + mobile viewing** — Android/Windows/macOS viewers join with zero extra setup; multi-window relay is sharded per window across worker threads.
- **Adaptive quality** — configurable scale, JPEG quality, framerate, bitrate, plus built-in presets.

### 🔐 Permissions

Four levels, per player per window — `NONE` (invisible) < `VIEW` (see it) < `INTERACT` (send mouse/keys) < `CONTROL` (move/resize/manage). The owner is always `CONTROL` on their own windows. Effective access is composed server-side in a fixed priority:

```
owner(CONTROL) > blacklist(NONE) > whitelist > per-window grant > default(NONE)
```

New viewers are governed by the **default policy** (`/wl share default …`):

| Policy | Behavior for **public** windows | Behavior for **targeted** windows |
|--------|--------------------------------|----------------------------------|
| `allow` *(default)* | auto-granted `VIEW` — the window appears with no owner action | never auto-granted (invite-only) |
| `ask` | owner gets a clickable invite per viewer (allow-10m / always / deny); **nothing is auto-approved** | only manual `grant` |
| `deny` | nothing auto-granted; only manual `grant` works | only manual `grant` |

> **Back-compat note:** the shipped default is `allow`, i.e. a public shared window is *immediately visible to every online player* with no approval step — same as prior versions. Switch to `ask`/`deny` to require approval. An explicit owner `revoke` writes a `NONE` tombstone that **outranks** the default policy, so auto-visibility never overrides a manual revocation.

CLI, all via `/wl share`:
```
grant  <win> <player> <PERM> [10m|1h|2d]   grant (timed; no duration = permanent)
revoke <win> <player>                      revoke (tombstone; survives default re-derivation)
default   allow|ask|deny                   set the new-viewer policy above
acl      [<win>]                           list grants: who / level / time-left / source
```

Hardening: timed expiry swept server-side (revoked windows dismantle instantly on viewers), handle-reuse wipes stale ACLs, and hostile inputs (levels, sizes, rates) are clamped — locked by `ShareAcl*Test` (37 tests).

### 🧩 Works without a server mod

On vanilla servers the mod enters **local direct mode**: window items place, render and interact entirely client-side; sharing degrades with an explicit explanation instead of silently failing. Items never disappear because of a missing server mod.

### ⚡ Performance Engineering

- PBO async readback + GPU scaling (`glBlitFramebuffer`)
- Diff-frame transfer: only changed regions are encoded; scratch buffers reused (no per-frame multi-MB allocations)
- Heartbeat frames for idle windows; `PNG`/`JPEG` auto-switch for transparency
- Oversized frames are degraded by JPEG quality only — **UI size never changes** (when a window has transparent pixels, oversize frames are force-encoded to JPEG with alpha blended onto black, so degradation actually works)
- Input hot paths measured, not guessed: key latency histogram (`/wl ime info`), popup frame delivery dedup (−97% JNI deliveries), buffered diagnostics logging (−98% write syscalls)
- Single-owner-thread PipeWire model — cross-thread UAF crashes eliminated, verified by 64+16-round live stress

### 🎨 Shader Compatible

Detects Iris and falls back to the vanilla entity pipeline automatically — windows display correctly even with shaders enabled, always at full brightness.

---

### 🗺️ Platform Support

| Platform | Capture local windows | View shared windows | Download |
|----------|:---:|:---:|---|
| Linux x86_64 | ✅ | ✅ | `waylandcraft-linux-x86_64.jar` |
| Linux arm64 | ✅ | ✅ | `waylandcraft-linux-arm64.jar` |
| Android x86_64 (viewer) | ❌ | ✅ | `waylandcraft-android-x86_64.jar` |
| Android arm64 (viewer) | ❌ | ✅ | `waylandcraft-android-arm64.jar` |
| Windows x86_64 (viewer) | 🟡 native backend merged, packaging in progress | ✅ | `waylandcraft-windows-x86_64.jar` |
| Windows arm64 (viewer) | 🟡 native backend merged, packaging in progress | ✅ | `waylandcraft-windows-arm64.jar` |
| macOS x86_64 (viewer) | 🟡 native backend merged, packaging in progress | ✅ | `waylandcraft-macos-x86_64.jar` |
| macOS arm64 (viewer) | 🟡 native backend merged, packaging in progress | ✅ | `waylandcraft-macos-arm64.jar` |
| iOS arm64 (viewer, experimental) | ❌ | ✅ | `waylandcraft-ios-arm64.jar` |
| Dedicated server (any arch) | — | — (hosts give/share/permission logic) | `waylandcraft-server.jar` |
| Universal (any platform, catch-all) | ✅ | ✅ | `waylandcraft-universal.jar` |

> **One jar per platform/arch.** Grab the exact file for your device from
> [Releases](https://github.com/scapking/waylandcraft/releases) — each platform jar
> carries only its own native payload, so Windows/macOS/iOS builds are slim
> viewer-only jars (~0.4 MB) that auto-detect the platform and disable local
> capture. Not sure which one? `waylandcraft-universal.jar` bundles every native
> platform as a catch-all; dedicated servers can use the lean pure-Java
> `waylandcraft-server.jar`.

- **Full mode (Linux)** — capture, share and view.
- **Viewer-only mode (Android / Windows / macOS)** — install the jar for your platform; the mod auto-detects the platform, disables local capture, and keeps receiving shared windows.
- **iOS (viewer, experimental)** — run Minecraft Java Edition + Fabric via [PojavLauncher](https://github.com/PojavLauncherTeam/PojavLauncher_iOS) (or its successor [Amethyst](https://github.com/AngelAuraMC/Amethyst-iOS)), then install `waylandcraft-ios-arm64.jar`. Not yet field-tested.
- **Server** — install `waylandcraft-server.jar` (pure Java, no native payload) on a dedicated server; the server-side give/permission/sharing logic is also bundled in every other jar, so a client jar on the server works too.
- **Universal** — `waylandcraft-universal.jar` bundles all native platforms (linux-gnu + android × x86_64/arm64). Use it when you don't know your exact platform; it is larger (~6 MB) than the single-platform jars.

---

## 🚀 Quick Start

### Prerequisites

- Minecraft **26.1.2** (Java Edition)
- Fabric Loader **0.19.x** + Fabric API **0.147.0+26.1.2**
- **Java 25**
- Linux with a **Wayland** session for full mode (capture requires Wayland; X11-only sessions are not supported) — Windows/macOS/Android run in viewer-only mode, see [Platform Support](#️-platform-support)

### Installation

1. Install Fabric Loader and Fabric API.
2. Drop the jar for your platform/arch (from [Releases](https://github.com/scapking/waylandcraft/releases), see [Platform Support](#️-platform-support)) into `.minecraft/mods/` — e.g. `waylandcraft-linux-x86_64.jar` on desktop Linux, `waylandcraft-android-arm64.jar` on a phone, `waylandcraft-macos-arm64.jar` on Apple Silicon.
3. **Multi-player:** with the mod on the server you get sharing, permissions and cross-player windows. **Without it**, the mod still works fully client-side in *local direct mode* (items, placement, interaction) and tells you exactly what multiplayer needs.
4. Launch the game — single-player worlds embed a server and share the same `mods/` folder.

### First Steps

```text
/wl launch firefox              # Launch an app (or press V)
/wl list windows                # List windows; 4-char random aliases shown at line end
/wl give <handle>               # Turn the window into an item; right-hold to place in the world
/wl grab <handle>               # Grab the window and drag it around (G toggles keyboard capture)
/wl share start <handle>        # Share it with your teammates
```

> 💡 **Android viewer?** Install `waylandcraft-android-<arch>.jar` (arm64 for most phones) and join the server — shared windows appear automatically, no configuration needed.

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
| Show / hide in world | `/wl show <handle|all>` / `/wl hide <handle|all>` |
| Pin (always visible) | `/wl pin <handle>` / `/wl unpin <handle>` |
| Terminate app | `/wl close <handle>` |
| Resize window | `/wl resize <handle> <w> <h>` |
| Position / inspect | `/wl pos <handle>` |
| Move (abs or `~` relative) | `/wl move <handle> <x> <y> <z>` |
| Rotate (degrees) | `/wl rotate <handle> <angle>` |
| List X11 desktop windows | `/wl x11 list` |
| Share an X11 window directly | `/wl x11 share <index>` |
| Stop X11 sharing | `/wl x11 stop <handle>` |

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

### Auto Layout (cube / sphere)

Windows can be arranged automatically around a fixed initialized origin (they no longer follow the player). Disabled by default.

| Command | Purpose |
|---------|---------|
| `/wl layout init [<x> <y> <z> [<yaw>]]` | Initialize layout center + yaw (no args = player position) |
| `/wl layout cube` | Switch to cube template (4 faces × N windows per face) and enable |
| `/wl layout sphere` | Switch to sphere template (VR screen-wall ring, stack upward) and enable |
| `/wl layout on` / `off` / `toggle` | Enable / disable / toggle auto layout |
| `/wl layout status` | Show template, center, radius, spacing, core window |
| `/wl layout list` | List windows in the layout (`➤` marks the core window) |
| `/wl layout add <handle>` / `remove <handle>` | Manually add / remove a window (when `layoutAutoJoin` is off) |
| `/wl layout core <handle>` | Set the core window explicitly |

* `Ctrl` + arrow keys moves the **core marker** to the neighbour window in that direction — any window can become the core (left/right wrap around, up/down cross layers, unlimited). The core window is highlighted with a cyan outline in-world. When auto layout is disabled, `Ctrl` + arrows still moves the hovered window manually.
* `G` captures the keyboard; the default key `H` toggles the cursor (both rebindable in the vanilla key settings).

### Sharing

| Command | Purpose |
|---------|---------|
| `/wl share start <handle> [player]` | Start sharing; optional `player` = **targeted** (only that player, never auto-visible to others). `all` / `*` = every window |
| `/wl share stop <handle>` | Stop sharing (`all` / `*` = stop all) |
| `/wl share quality <handle> <s> <q> <fps>` | Set scale / quality / framerate |
| `/wl share preset <handle> <preset>` | Apply a preset (see [Configuration](#️-configuration)) |
| `/wl share config <handle> <param> <value>` | Tune a single parameter |
| `/wl share reset <handle>` | Reset to defaults |
| `/wl share info <handle>` | Show current share config |
| `/wl share resolution <handle> <w> <h>` | Set target resolution |
| `/wl share stats <handle>` | Show share statistics |

### Permissions

Two families: **per-window owner controls** (`/wl share …`, "who shares, who manages") and **server-wide tables** (`/wl permission …`, ops-level, outrank the default policy). Both feed the single compose chain shown in [Permissions](#-permissions).

| Command | Scope | Purpose |
|---------|-------|---------|
| `/wl share grant <win> <player> <PERM> [dur]` | owner | grant; optional duration `10m`/`1h`/`2d` (no dur = permanent, cap 30d) |
| `/wl share revoke <win> <player>` | owner | revoke (NONE tombstone; survives default re-derivation) |
| `/wl share default allow\|ask\|deny` | owner | new-viewer policy for public windows (see table above) |
| `/wl share acl [win]` | owner/self | audit: who / level / time-left / source |
| `/wl permission list` | ops | list server whitelist/blacklist/default |
| `/wl permission default <PERM>` | ops | server-wide default VIEW-level for public windows |
| `/wl permission allow <player> <PERM>` | ops | whitelist (bypasses per-window, still not owner) |
| `/wl permission deny <player>` | ops | blacklist (hard NONE, even owner-granted) |
| `/wl permission remove <player>` | ops | drop the player from the tables |

`PERM`: `NONE` / `VIEW` / `INTERACT` / `CONTROL`. Durations accept `s/m/h/d` suffixes or bare seconds.

---

### Diagnostics & IME

| Command | Purpose |
|---------|---------|
| `/wl status` | One-screen dashboard: every subsystem (bridge/IME/audio/capture/EGL), colored health, error ring, live telemetry |
| `/wl ime info` | IME deep state: daemon phase, seeded engines, source mode, candidate-window delivery stats, key latency histogram |
| `/wl ime nested start\|stop` | Control the nested IM daemon |
| `/wl ime source auto\|fcitx5\|builtin` · `/wl ime eng` | Choose IME source / list & pick engines |

## 📚 Command Reference

### Settings

| Command | Purpose |
|---------|---------|
| `/wl settings list` | Show current settings |
| `/wl settings set <key> <value>` | Change a setting |

### Share Quality Parameters

`/wl share config <handle> <param> <value>` accepts:

| Parameter | Description | Range |
|-----------|-------------|-------|
| `scale` | Resolution scale | 0.1 – 1.0 |
| `quality` | JPEG quality | 0.1 – 1.0 |
| `fps` | Max framerate | 5 – 120 |
| `bitrate` | Max bitrate (kbps) | 0 = unlimited |
| `diffThreshold` | Pixel-change threshold | 0.001 – 1.0 |
| `diff` | Diff-frame transfer on/off | true / false |
| `buffer` | Frame buffer count | 1 – 8 |
| `latency` | Latency compensation (ms) | 0 – 500 |
| `prediction` | Motion prediction on/off | true / false |
| `compression` | Compression method | e.g. `lz4` / `zlib` / `none` |

### Presets

| Preset | Scale | Quality | FPS | Bitrate |
|--------|-------|---------|-----|---------|
| `performance` | 0.25 | 0.5 | 60 | 1000 kbps |
| `balanced` | 0.5 | 0.7 | 30 | 2000 kbps |
| `quality` | 1.0 | 1.0 | 30 | unlimited |
| `lowlatency` | 0.35 | 0.6 | 60 | 1500 kbps |

### X11 Window Sharing

Share windows from the X11 desktop (via `xwayland-satellite`) directly, without a Wayland toplevel:

| Command | Purpose |
|---------|---------|
| `/wl x11 list [display]` | List X11 desktop windows (defaults to satellite display) |
| `/wl x11 share <index>` | Share the Nth window from the list |
| `/wl x11 stop <handle>` | Stop sharing an X11 window |

### Global Settings

`/wl settings set <key> <value>` accepts all of the following (also visible via `/wl settings list`):

| Key | Default | Description |
|-----|---------|-------------|
| `pixelsPerBlock` | `500` | Pixel density of a window per block |
| `windowAntialiasing` | `true` | RGSS antialiasing (no shaders only) |
| `focusOnHover` | `false` | Auto-focus window on hover |
| `hideCursor` | `false` | Hide the virtual mouse cursor while controlling a window |
| `layoutEnabled` | `true` | Auto-layout enabled by default (v0.2.37 behavior; auto-inits at player position) |
| `layoutAutoJoin` | `true` | New windows auto-join the layout (false = only `/wl layout add` windows) |
| `layoutTemplate` | `cube` | Layout template: `cube` or `sphere` |
| `layoutInitialized` | `false` | Whether `/wl layout init` was run (layout unusable until set) |
| `layoutInitX` / `Y` / `Z` | `0.0` | Layout center coordinates |
| `layoutInitYaw` | `0.0` | Layout facing (degrees, 0 = toward +Z, clockwise) |
| `layoutRadius` | `6.0` | Layout radius in blocks (center → window horizontal distance) |
| `layoutSpacing` | `0.4` | Min horizontal spacing between windows on a layer (blocks) |
| `layoutStackSpacing` | `0.4` | Vertical spacing between layers (blocks) |
| `layoutCubePerFace` | `2` | Cube template windows per face (4 faces → 8 total) |
| `layoutDefaultWidth` | `1080` | Resolution applied to windows joining the layout |
| `layoutDefaultHeight` | `540` | Resolution applied to windows joining the layout |
| `groundClearance` | `0.4` | Min clearance of window bottom above floor (blocks) |
| `moveStep` | `0.5` | Manual move step for Ctrl+arrow key movement |
| `sharePlayoutDelayMs` | `auto` (-1) | Viewer playout buffer: `auto` = load-adaptive 14–32 s (congestion deepens, 30 s clean relaxes); `0` = legacy low-latency; `1–32000` = manual ms |
| `audioShareRangeBlocks` | `32` | Distance falloff radius for shared-window audio (0 = audible everywhere) |
| `imeSource` | `auto` | IME engine source: `auto` / `fcitx5` / `builtin` |
| `verboseLogging` | dev-env default | Full diagnostic logging on/off (release builds stay quiet; see [docs/LOGGING.md](docs/LOGGING.md)) |

---

## 🎨 Shader Compatibility

- With Iris shaders enabled, windows render through the **vanilla entity pipeline** at full brightness — unaffected by shader lighting.
- Without shaders, a custom pipeline is used with optional RGSS antialiasing (`windowAntialiasing`).
- Both modes render the window front face textured, **back face solid black** — identical behavior.

---

## 💡 Tips & Best Practices

- **OpenGL hooking tools** — tools like MangoHud or OBS game capture that hook into OpenGL can prevent the mod from fully initializing; launch without them if windows fail to appear.
- **Windows stay vertical** — windows are always upright; the vertical axis is locked while dragging and the bottom edge never goes below **0.4 blocks** above the floor. `Ctrl+Scroll` rotates the facing (still vertical).
- **Precise placement** — read the current pose with `/wl pos <handle>`, then set exact coordinates with `/wl move` (`~` relative offsets supported) and facing with `/wl rotate`.
- **Server must run the mod** — in multiplayer, `give` / `permission` / `share` depend on server-side logic; without the mod on the server, requests are silently dropped.
- **Slight aliasing** at rounded corners/shadows is normal for JPEG; transparent windows automatically use PNG to preserve alpha.
- **Full help in-game**: `/wl help`.

---

## ❓ FAQ

**Q: Does the server need the mod?**
A: For multiplayer sharing/permissions — yes, and it ships as the pure-Java `waylandcraft-server.jar`. **Without it the mod no longer breaks**: local direct mode keeps items, placement and interaction fully working client-side, and `/wl share` explains what a server mod would add.

**Q: Can I type Chinese/Japanese in the game?**
A: Yes — pinyin/wubi/shuangpin/… with the **in-game candidate window**, both in Minecraft's own chat and inside in-game app windows. Best with `fcitx5` installed (every engine auto-enabled); with nothing installed the built-in pinyin engine covers the basics. See [docs/IME.md](docs/IME.md).

**Q: Can I view shared windows on my phone?**
A: Yes. Install `waylandcraft-android-<arch>.jar` and join the server. Windows shared by PC teammates appear automatically — no extra steps on the phone.

**Q: Shared windows stutter on my network.**
A: Keep the default `auto` — the viewer playout buffer extends itself from 14 s up to 32 s under congestion evidence (buffer-cap truncation, decode stalls, late audio) and relaxes back after 30 s clean: `/wl settings set sharePlayoutDelayMs auto`. Prefer a fixed depth? `/wl settings set sharePlayoutDelayMs 20000` (range up to 32000; 0 = legacy low-latency). The buffer trades latency for zero stutter without touching resolution.

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
3. **IME nuances that remain (down from "unusable" to listed edges)** — the nested-daemon architecture resolved CJK input entirely on fcitx5-equipped systems (candidate window, all engines, GUI + world windows). Remaining edges: (a) on **ibus-only** GNOME defaults, only the built-in pinyin engine is available — install fcitx5 for other engines (ibus ships no Wayland input-method v2 client; this is upstream-of-upstream, not our gap); (b) IME engines that render candidates exclusively via dmabuf popups are not yet captured (fcitx5 classicui uses shm — works); (c) candidate anchoring inside *nested app sub-surfaces* (popup-based input fields) can offset by the sub-surface origin. See [docs/IME.md](docs/IME.md) §compatibility.

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
| Native Bridge | Rust, JNI (panic-safe exports, version-injected metadata) |
| Wayland | Smithay nested compositor, wayland-client, wlr-foreign-toplevel-management |
| Input methods | `zwp_text_input_v3` + `zwp_input_method_v2` servers, virtual keyboard, nested fcitx5/ibus daemon supervisor, built-in pinyin engine, popup ARGB capture → GUI/world candidate rendering |
| Audio | PipeWire per-PID capture (single owner-thread), OpenAL playout synced to video clock |
| Capture | KDE/Wallpaper portal, XComposite/XGetImage, Windows.Graphics.Capture (merged), ScreenCaptureKit (merged) |
| Image | PBO async readback, glBlitFramebuffer, JPEG/PNG, MemoryUtil |
| Network | Fabric Networking API, protocol **v2** payloads (seq + capture timestamps), server relay with canSend filtering + write backpressure + sharded threads |

**Test culture**: three tiers — pure-logic unit tests (251 Rust / 153 Java), wire-level integration tests that run a real compositor with a mock fcitx5 client, and `#[ignore]` **live suites** (real fcitx5 end-to-end commit/candidates/engine-switching, real PipeWire topology + stress). Release rule: a server jar is smoke-booted in an isolated server before publication; protocol changes ship all three endpoints in one batch.

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

- **1.4.0 (fork series sync3–15)** — the productization arc:
  - **IME endgame** — nested fcitx5 daemon with auto-seeding of all installed engines; commit delivery hardened to protocol truth; **in-game candidate window** (GUI *and* world-space quads beside app windows, click + wheel); **built-in pinyin engine** for zero-dependency Chinese input; 「拼」state badge; `/wl ime info` / `/wl ime source` / `/wl ime eng`. Every field-reported failure mode became a regression test, including real-fcitx5 live suites.
  - **Stutter-free sharing** — playout-buffered A/V (4s default, clock-offset aware), per-window application audio (per-PID PipeWire capture, watchdog re-promotion), resolution never downscaled.
  - **Crash elimination** — PipeWire single-owner-thread threading model (cross-thread UAF gone, 80-round live stress), JNI-boundary UB fixes, portal capture stop-chain (no more zombie capture threads / stuck screen-record indicator).
  - **Security hardening (27 audit findings)** — two remote server-crash P0s closed (permission-command clamp + auth, give_items thread/DoS), viewer backpressure + capability filtering, invite-based timed grants with revocation propagation, handle-reuse ACL wipe, input clamps end-to-end.
  - **Cross-platform capture backends** — Windows (Graphics.Capture) and macOS (ScreenCaptureKit) natives merged at Rust level with module-level cross-compilation gates; Java wiring and native CI packaging in progress.
  - **Diagnostics** — `/wl status` one-screen subsystem dashboard, error ring, key-latency histograms, structured ACL audit, release-vs-verbose log tiers.
- **v0.9.27** — **IME protocol-level rewrite.** Rebuilt the passthrough onto the modern `zwp_text_input_v3` + `zwp_input_method_v2` stack (legacy v1 paths removed) around a pure-logic relay state machine (`ime/relay.rs`) with correct `done`/`commit(serial)` accounting, atomic batching and protocol-discard semantics — the reason IME text now reliably reaches apps. See [docs/IME.md](docs/IME.md).
- **v0.9.17** — Fixed shared-window audio (a dropped `StreamListener` handle meant no PCM was ever captured); full audio-pipeline logging.
- **v0.9.16** — `/wl show all` / `/wl hide all` (also `*`).
- **v0.9.15** — Layout order now updates in real time as windows close/open — slots are compactly renumbered to fill gaps.
- **v0.9.14** — Ctrl+arrow now swaps positions with the adjacent window (swapCore); fixed inverted left/right direction.
- **v0.9.13** — Ctrl+arrow restored to moving the hovered window (manualOffset survives per-frame layout re-anchoring).
- **v0.9.12** — `/wl share start all` one-click share every window (`stop all` likewise); Ctrl+arrow switches the layout "core" marker.
- **v0.9.11** — Fixed the root cause of keyboard passthrough: `xkb_state.update_key` was fed evdev codes (`key-8`) but xkbcommon requires xkb keycodes (evdev+8). The invalid keycodes were silently ignored, so modifier bits (Ctrl/Shift/Alt) never set — single keys looked fine but every shortcut (Ctrl+L etc.) failed. `mods(depressed=0)` observed in kb.log with Ctrl held down was the smoking gun. **Combination keys now pass through correctly.**
- **v0.9.10** — Fixed `setKbLogFileNative` JNI registration name mismatch (Rust macro snake→camel auto-generation vs explicit name).
- **v0.9.9** — Rust keyboard log now writes to its own file `waylandcraft-kb.log` (setKbLogFile) for easy upload & diagnosis of focus/send state.
- **v0.9.8** — Fixed the main keyboard passthrough root cause: `correctScancode` no longer adds +8 on Wayland (double keycode offset).
- **v0.9.7** — Log noise reduction; `keyboard_key` now logs modifier state (mods summary per key); tick focus log throttled; `keyboard_focus` idempotent-silent.
- **v0.9.6** — Full-pipeline keyboard debug logging (mixin entry / onKeyPress branch / local forward / bridge / Rust focus / per-key send).
- **v0.9.5** — Fixed local window keyboard passthrough (scenario B): focus fallback + forward self-healing + diagnostic logs.
- **v0.9.4** — Fixed Ctrl+arrow direction inverted + J false-trigger while G-bound.
- **v0.9.3** — Shared window long-press REPEAT passthrough (`forwardSharedKey` Repeat branch; requirement 1 completion).
- **v0.9.2** — Ctrl+arrows now swap layout order (plan A) — no range limits.
- **v0.9.1** — Fixed all-keyboard-dead after G binding — set keyboard focus (focusSurface) on bind/hover.
- **v0.9.0** — 键盘输入子系统重构（方案 C）：长按 REPEAT 事件完整透传（修复长按失效）；组合键/大小写由 Rust xkb 状态机全权维护，Java 只做透传；Ctrl+方向键**永远移动窗口**（恢复 v0.2.37 语义，布局核心切换解绑）；捕获分工 **G=纯键盘、J=键盘+鼠标**；release 自动生成按版本变更描述。
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
