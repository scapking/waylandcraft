# WaylandCraft 🎮🪟

**Minecraft の中で Linux デスクトップアプリを動かす** — Wayland compositor を Minecraft に統合する Fabric mod。ゲームワールド内で Linux デスクトップのウィンドウを表示・操作でき、マルチプレイでのウィンドウ共有にも対応しています。

> ⚠️ このプロジェクトは元の [WaylandCraft](https://github.com/EVV1E/waylandcraft.git) をベースに、マルチプレイ表示などの機能を AI 支援で実装したものです。**機能・安全性は保証されません。自己責任でご利用ください。**

<p align="center">
  <img src="https://img.shields.io/badge/Minecraft-26.1.2-green" />
  <img src="https://img.shields.io/badge/Fabric%20Loader-0.19.2+-blue" />
  <img src="https://img.shields.io/badge/Fabric%20API-0.147.0%2B-blue" />
  <img src="https://img.shields.io/badge/Java-25-orange" />
  <img src="https://img.shields.io/badge/Version-v0.2.32-brightgreen" />
</p>

---

## ダウンロード

👉 **[最新リリース (v0.2.32)](https://github.com/scapking/waylandcraft/releases/latest)** — `waylandcraft.jar` をダウンロードして `mods/` フォルダに入れてください。

> 上流リポジトリ（almightydb）の Releases ページは更新が遅れています。最新版は上記リンクから取得してください。

---

## バージョンハイライト（v0.2.32）

- **サーバー側マルチスレッドフレーム転送** — フレームをウィンドウハンドルごとに N スレッドへ分散（同一ウィンドウは順序維持、別ウィンドウは並列転送）。登録/登録解除はサーバーメインスレッドで実行され、netty スレッドがリストブロードキャストでブロックされません。
- **品質低下が確実に機能** — 透明ピクセルを含むウィンドウが PNG（可逆・品質無効）のまま停滞しなくなりました。サイズ上限超過時は強制的に JPEG（透明は黒背景に合成）へ変換され、画質のみ低下し UI サイズは変わりません。
- **1 フレーム上限の緩和** — 600 KB → 1.8 MB（サーバープロトコル上限に整合）。通常の高解像度ウィンドウではフレームが破棄されなくなりました。

---

## 機能

| 機能 | 説明 |
|------|------|
| 純 CLI モード | SF 風 UI を廃止しバニラ描画に回帰。操作はすべて `/wl` コマンド |
| ワールド内ウィンドウ | Wayland ウィンドウをゲームワールドに表示。ドラッグ・リサイズ・ピン留め・非表示 |
| 統一レンダリング | ローカルとリモート共有ウィンドウで同一の描画経路を使用 |
| マルチプレイ共有 | ウィンドウを他プレイヤーに共有し、相手のワールドにリアルタイム描画 |
| デスクトップキャプチャ | XDG Desktop Portal + PipeWire によるウィンドウ取得 |
| 権限管理 | 4 段階: NONE / VIEW / INTERACT / CONTROL |
| Iris（シェーダー）互換 | Iris 導入時は自動的にバニラパイプラインへフォールバックし、シェーダー有効でも正常表示 |
| 適応画質 | スケール・JPEG 品質・フレームレート・ビットレートを設定可能。プリセット内蔵 |
| パフォーマンス | PBO 非同期リードバック、GPU スケーリング、差分フレーム転送、ハートビート、PNG/JPEG 自動切替、サーバー側マルチスレッドフレーム転送（メインスレッド非占有） |

---

## デモスクリーンショット

<p align="center">
  <img src="assets/demo_1.jpg" width="49%" alt="Demo 1" />
  <img src="assets/demo_2.jpg" width="49%" alt="Demo 2" />
</p>

> 上記の画像はデモ例です。

---

## インストール

### 要件

- Minecraft **26.1.2**（Java Edition）
- Fabric Loader **0.19.x** + Fabric API **0.147.0+26.1.2**（または対応バージョン）
- **Java 25**
- Linux + **Wayland** セッション（ウィンドウキャプチャはネイティブライブラリが担当。X11 は非対応）
- **xwayland-satellite 同梱** — X11 アプリに `DISPLAY` を自動提供。手動インストール不要（システムの `Xwayland` は必要。ほぼ全ての Wayland デスクトップに含まれます）。jar には **x86_64 と arm64** 両方のバイナリが同梱されており、ARM64 ホスト（Raspberry Pi など）も追加設定なしで動作します。

### 手順

1. Fabric Loader と Fabric API をインストール
2. `waylandcraft.jar` を `.minecraft/mods/` に配置
3. **マルチプレイの場合、サーバー側にも mod の導入が必要**（`give` / `permission` / `share` はサーバー側に登録されており、未導入だと `/wl give` などが静かに失敗します）
4. ゲームを起動（シングルプレイのワールドは内蔵サーバーで、クライアントとサーバーで `mods/` を共用します）
5. **Android（閲覧専用）**：`waylandcraft-android-<arch>.jar` を導入（ネイティブ Wayland が無い場合はローカル機能が自動無効化され、クラッシュしません）。この mod が入ったサーバーに参加すると、デスクトップ側のプレイヤーが共有したウィンドウ（`/wl share start <handle>`）をそのまま表示できます

---

## クイックスタート

1. アプリを起動: `/wl launch firefox`
2. ウィンドウ一覧: `/wl list windows`
3. ウィンドウをアイテム化: `/wl give <handle>` → **右クリック長押し**でワールドに設置
4. キーボードを捕獲して操作: `/wl grab <handle>`（または `G` キーで切替）
5. 共有: `/wl share start <handle>`

### キーバインド

| キー | 機能 |
|------|------|
| `B` | ウィンドウマネージャ画面を開く |
| `G` | キーボード捕獲 / 解放を切替（捕獲中はキーがウィンドウに透過） |
| `右クリック長押し` + WindowItem | ウィンドウをワールドに表示 |

> その他の操作はすべて `/wl` コマンドで行います。一覧は下記参照。

---

## コマンド

`<handle>` は 4 形式に対応: `0x` 短ハンドル / 完全ハンドル / **インスタンス別名 `wN`（`/wl list windows` で表示、セッション内で一意）** / アプリ別名（例 `firefox_esr`）。同名ウィンドウが複数ある場合は `別名:N`（例 `firefox:2`）で指定します。

### ウィンドウ管理

| コマンド | 機能 |
|----------|------|
| `/wl list` | 起動可能なアプリ一覧（デフォルト） |
| `/wl list windows` | compositor 内のウィンドウ一覧 |
| `/wl list apps` | 起動可能なアプリ一覧 |
| `/wl list desktop` | キャプチャ可能なデスクトップウィンドウ一覧 |
| `/wl launch <app>` | アプリを起動（名前/完全別名で指定。同プレフィックスのアプリは完全別名で区別、例 `visual_studio_code`） |
| `/wl capture` | Portal 選択を開いてデスクトップウィンドウをキャプチャ |
| `/wl give <handle>` | ウィンドウをアイテム化してインベントリへ |
| `/wl take <handle>` | ウィンドウアイテムを回収 |
| `/wl grab <handle>` | ウィンドウを掴む（マウスでドラッグ、ホイールで前後移動） |
| `/wl show <handle>` | ウィンドウをワールドに表示 |
| `/wl hide <handle>` | ワールドから非表示 |
| `/wl pin <handle>` | ピン留め（非表示・最小化の影響を受けず表示維持） |
| `/wl unpin <handle>` | ピン留め解除 |
| `/wl close <handle>` | アプリを終了（ウィンドウを閉じる） |
| `/wl resize <handle> <w> <h>` | ウィンドウ解像度を変更 |
| `/wl pos <handle>` | ウィンドウの位置（x/y/z）、向き角度、拡大率、解像度を表示 |
| `/wl move <handle> <x> <y> <z>` | ウィンドウ座標を設定（絶対値 `100.5`、または相対オフセット `~0.5` / `~-1` / `~`） |
| `/wl rotate <handle> <angle>` | ウィンドウの向き角度を設定（度、Y 軸回り。絶対値 `90`、または相対 `~15`。`0`=+Z 向き, `90`=+X 向き） |
| `/wl template save <name>` | 現在のチャンク内の全ウィンドウ配置を一時テンプレートとして保存（再起動で消える） |
| `/wl template savep <name>` | 永続テンプレートを保存（アプリ + 位置 + 解像度、ディスクへ書き込み） |
| `/wl template apply <name>` | 一時テンプレートを適用し、ウィンドウ位置を復元 |
| `/wl template applyp <name>` | 永続テンプレートを適用：アプリを自動起動して記録どおりに配置 |
| `/wl template list` | 全テンプレートを一覧表示 |
| `/wl template remove <name>` / `removep <name>` | 一時 / 永続テンプレートを削除 |

### 共有管理

| コマンド | 機能 |
|----------|------|
| `/wl share start <handle>` | 共有開始 |
| `/wl share stop <handle>` | 共有停止 |
| `/wl share quality <handle> <s> <q> <fps>` | 画質設定（スケール・品質・fps） |
| `/wl share preset <handle> <preset>` | プリセット適用（下記参照） |
| `/wl share config <handle> <param> <value>` | 単一パラメータ設定 |
| `/wl share reset <handle>` | 画質をデフォルトに戻す |
| `/wl share info <handle>` | 現在の共有設定を表示 |
| `/wl share resolution <handle> <w> <h>` | 目標解像度を設定 |
| `/wl share stats <handle>` | 共有統計を表示 |

### 権限管理

| コマンド | 機能 |
|----------|------|
| `/wl permission list` | 権限一覧 |
| `/wl permission default <PERM>` | デフォルト権限を設定 |
| `/wl permission allow <player> <PERM>` | ホワイトリスト追加 |
| `/wl permission deny <player>` | ブラックリスト追加 |
| `/wl permission remove <player>` | プレイヤーを削除 |

> `PERM`: `NONE` / `VIEW` / `INTERACT` / `CONTROL`

### 設定

| コマンド | 機能 |
|----------|------|
| `/wl settings list` | 現在の設定一覧 |
| `/wl settings set <key> <value>` | 設定変更 |

| パラメータ | デフォルト | 説明 |
|-----------|-----------|------|
| `pixelsPerBlock` | `500` | ワールド内 1 ブロックあたりのウィンドウ画素密度 |
| `windowAntialiasing` | `false` | ウィンドウ RGSS アンチエイリアス（カスタムパイプライン時のみ） |
| `focusOnHover` | `false` | マウスホバーで自動フォーカス |

### 共有パラメータ & プリセット

| パラメータ | 説明 | 範囲 |
|-----------|------|------|
| `scale` | 解像度スケール | 0.1 – 1.0 |
| `quality` | JPEG 品質 | 0.1 – 1.0 |
| `fps` | 最大フレームレート | 5 – 120 |
| `bitrate` | 最大ビットレート (kbps) | 0 = 無制限 |
| `diffThreshold` | 画素変化閾値 | 0.001 – 1.0 |

| プリセット | スケール | 品質 | FPS | ビットレート |
|-----------|---------|------|-----|------------|
| `performance` | 0.25 | 0.5 | 60 | 1000 kbps |
| `balanced` | 0.5 | 0.7 | 30 | 2000 kbps |
| `quality` | 1.0 | 1.0 | 30 | 無制限 |
| `lowlatency` | 0.35 | 0.6 | 60 | 1500 kbps |

---

## Iris（シェーダー）互換

- Iris シェーダー有効時もウィンドウは正常表示：Iris 検出時は**バニラ entity パイプライン**へ自動フォールバックし、ウィンドウ内容は常に最大輝度（シェーダー照明の影響なし）
- シェーダー無効時はカスタムパイプラインを使用し、RGSS アンチエイリアス（`windowAntialiasing`）が利用可能
- どちらのモードもウィンドウは**表面がテクスチャ、裏面が単色黒**で同一挙動

---

## 注意事項

- **ウィンドウは常に垂直**：ウィンドウは常に直立配置（傾け不可）、ドラッグ中は垂直軸（y）が固定され水平移動のみ、ウィンドウ下端はその地点の地面より常に **0.4 ブロック**以上上に保たれます。`Ctrl+ホイール` で向きを回転（垂直のまま）
- **精密な配置**：`/wl pos <handle>` で現在位置・角度を確認後、`/wl move <handle> <x> <y> <z>` で座標を（`~` 相対オフセット対応）、`/wl rotate <handle> <angle>` で向き角度（度）を正確に設定できます
- **サーバーにも mod の導入が必要**：マルチプレイではサーバー側機能（`give` / `permission` / `share`）がサーバー導入に依存し、未導入だとリクエストが黙って破棄されます
- 角丸・影のわずかなジャギーは JPEG の仕様です。透明ピクセルを含むウィンドウは PNG に自動切替されアルファを保持します（共有フレームがサイズ上限を超えた場合は強制的に JPEG へ：透明は黒背景に合成され、画質のみ低下し UI サイズは変わりません）
- デスクトップキャプチャ（`/wl capture`）は XDG Desktop Portal と Wayland セッションが必要です
- ゲーム内ヘルプ: `/wl help`

---

## 既知の制限（改善予定）

現行バージョンには以下の未整備な点があり、今後のリリースで改善予定です。

1. **ウィンドウ移動は意図的に制約** — ウィンドウは固定垂直で、ドラッグ中は高さ軸が固定されます（下端は地面より 0.4 ブロック以上上）。これは意図的な簡素化であり、より自由な配置は今後拡張予定です。
2. **共有の画質と遅延のトレードオフ** — 共有元と同一の UI サイズを維持するため、上限超過時は JPEG 品質のみを下げ解像度は変えません。高解像度ウィンドウは弱いサーバー/スマホでの転送・デコードに依然負荷がかかります。

---

## ビルド

```bash
# 要件: Java 25、Rust ツールチェーン、Wayland 開発ライブラリ
apt install libwayland-dev libxkbcommon-dev pkg-config libclang-dev

# 1. Rust ネイティブライブラリをビルド（release 必須。build.gradle は release .so を優先）
source ~/.cargo/env
cd native && cargo build --release

# 2. Java mod をビルド
cd .. && ./gradlew clean build

# 出力: build/libs/waylandcraft.jar（約 6.0MB、x86_64 と arm64 の xwayland-satellite を同梱）
```

> ⚠️ パッケージされるネイティブライブラリが `native/target/release/libwaylandcraft.so`（約 3.7MB）であることを確認してください。debug ビルド（176MB・デバッグシンボル付き）を誤って同梱すると jar が 39MB になります。

> 📦 同梱の `xwayland-satellite` バイナリは `native/build-satellite.sh` でビルドします——`x86_64` と `arm64` のそれぞれで一度ずつ実行し（arm64 クロスコンパイルは `aarch64-linux-gnu-gcc` + `cargo build --target aarch64-unknown-linux-gnu`）、両バイナリを `native/` に配置してください。

---

## 技術スタック

| レイヤー | 技術 |
|----------|------|
| ゲーム | Java 25, Fabric Loader 0.19.2+, Fabric API 0.147.0+ |
| ネイティブブリッジ | Rust, JNI |
| Wayland | Smithay, wayland-client, wlr-foreign-toplevel-management |
| 画像 | PBO 非同期リードバック, glBlitFramebuffer, JPEG/PNG, MemoryUtil |
| ネットワーク | Fabric Networking API, カスタム Payload プロトコル |

---

## ライセンス

MIT License

## 謝辞

- [WaylandCraft](https://github.com/EVV1E/waylandcraft.git) — オリジナルプロジェクト
- [Smithay](https://github.com/Smithay/smithay) — Wayland compositor フレームワーク
- [Fabric](https://fabricmc.net/) — Minecraft mod loader
