<p align="center">
  <a href="README_EN.md">English</a> | <a href="README_ZH.md">中文</a> | <a href="README_JA.md">日本語</a>
</p>

<p align="center">
  <h1 align="center">WaylandCraft 🎮🪟</h1>
  <p align="center"><b>Run Linux desktop apps inside Minecraft</b></p>
  <p align="center">
    <img src="https://img.shields.io/badge/Minecraft-26.1.2-green" />
    <img src="https://img.shields.io/badge/Fabric%20Loader-0.19.2+-blue" />
    <img src="https://img.shields.io/badge/Fabric%20API-0.147.0%2B-blue" />
    <img src="https://img.shields.io/badge/Java-25-orange" />
    <img src="https://img.shields.io/badge/Version-v0.2.32-brightgreen" />
  </p>
</p>

---

> ⚠️ This project is based on the original [WaylandCraft](https://github.com/EVV1E/waylandcraft.git). Multi-player display and other features were AI-implemented. **Functionality and security are NOT guaranteed.** Use at your own risk.

---

**Choose your language / 选择语言 / 言語を選択:**

- **[English](README_EN.md)**
- **[中文](README_ZH.md)**
- **[日本語](README_JA.md)**

---

## Download

👉 **[Latest Release (v0.2.32)](https://github.com/scapking/waylandcraft/releases/latest)** — Download `waylandcraft.jar` and drop it into your `mods/` folder.

> The upstream repository (almightydb) Releases page lags behind; grab the latest build from the link above.

## Highlights (v0.2.32)

- **Server-side multi-thread frame relay** — frames are sharded across N threads by window handle (same window keeps order, different windows relay in parallel); register/unregister run on the server thread, so the netty thread is never blocked by list broadcasts.
- **Reliable JPEG degrade** — windows with transparent pixels no longer get stuck in PNG (lossless, quality had no effect); oversize shared frames are force-encoded to JPEG with alpha blended on black, so quality degradation actually works.
- **Higher frame limit** — single-frame JPEG/PNG limit raised from 600 KB to 1.8 MB (aligned with the server protocol cap), oversize frames are no longer dropped for ordinary high-res windows.

## License

MIT License
