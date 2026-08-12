<p align="center">
  <h1 align="center">🎮🪟 WaylandCraft</h1>
  <p align="center"><b>Minecraft の中で本物の Linux デスクトップアプリを動かす。</b></p>
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
  <img src="https://img.shields.io/badge/Version-v0.9.16-brightgreen" />
  <img src="https://img.shields.io/badge/License-MIT-blue" />
</p>


> ⚠️ **免責事項** — 本プロジェクトはオリジナルの [WaylandCraft](https://github.com/EVV1E/waylandcraft.git) をベースにした二次開発です。マルチプレイヤー共有などの機能は AI によって実装されています。**機能と安全性は保証されません。** 利用は自己責任でお願いします。

---

## 目次

- [✨ 特徴](#-特徴)
- [🗺️ 対応プラットフォーム](#️-対応プラットフォーム)
- [🚀 クイックスタート](#-クイックスタート)
- [📖 使い方ガイド](#-使い方ガイド)
- [📚 コマンドリファレンス](#-コマンドリファレンス)
- [⚙️ 設定](#️-設定)
- [🎨 シェーダー互換性](#-シェーダー互換性)
- [💡 ヒントとベストプラクティス](#-ヒントとベストプラクティス)
- [❓ FAQ](#-faq)
- [🚧 既知の制限](#-既知の制限)
- [🏗️ ソースからのビルド](#️-ソースからのビルド)
- [🧱 アーキテクチャ](#-アーキテクチャ)
- [🤝 コントリビュート](#-コントリビュート)
- [📜 変更履歴](#-変更履歴)
- [📄 ライセンス](#-ライセンス)

---

## ✨ 特徴

### 🖥️ ゲーム内で本物の Linux アプリを実行

任意の Wayland ウィンドウをゲーム内オブジェクトに変換します。アプリの起動、仮想スクリーンでの表示、操作まで、Minecraft から離れることなく完結します。

- **純 CLI モード** — すべての操作は `/wl` コマンドで完結。バニラ描画に回帰し、SF 風 UI のノイズなし
- **統一描画** — ローカルウィンドウとリモート共有ウィンドウが同じ描画パスを通るため、見た目は完全一致
- **自由なウィンドウ配置** — ドラッグ・リサイズ・固定・非表示・回転が可能。テンプレートでレイアウトを保存/復元
- **完全なキーボード透過** — 単独キー・修飾キー（Ctrl/Shift/Alt）・長押し REPEAT がすべてフォーカス中のウィンドウへ到達。キャプチャ分担 **G=キーボードのみ、J=キーボード+マウス**
- **X11 アプリ対応** — 同梱の `xwayland-satellite` が X11 アプリに `DISPLAY` を自動提供（`x86_64` と `arm64` の両アーキテクチャ対応）

### 👥 マルチプレイヤーでのウィンドウ共有

デスクトップをリアルタイムで他のプレイヤーにストリーミング。サーバーサイドの第一級機能として実装されており、後付けではありません。

- **リアルタイム共有** — 他のプレイヤーの世界にあなたのウィンドウが表示されます
- **モバイル視聴** — Android クライアントなら追加設定なしで共有ウィンドウを表示
- **サーバーサイド中継** — フレーム転送を Server スレッドから分離。複数ウィンドウの共有はワーカースレッドに分散され、遅い視聴者が他のウィンドウを妨げません
- **適応的画質** — スケール・JPEG 品質・フレームレート・ビットレートを設定可能。内蔵プリセットも用意

### 🔐 きめ細かい権限

プレイヤー × ウィンドウごとの 4 段階権限モデル：

| レベル | 意味 |
|--------|------|
| `NONE` | ウィンドウはプレイヤーから見えず、存在も知られない |
| `VIEW` | 共有ウィンドウを世界内で見られる |
| `INTERACT` | ウィンドウへマウス/キーボードイベントを送れる |
| `CONTROL` | サイズ・位置を変更し、権限を管理できる |

ホワイトリスト・ブラックリスト・ウィンドウ単位の上書きにも対応。

### ⚡ パフォーマンス設計

- PBO 非同期リードバック + GPU スケーリング（`glBlitFramebuffer`）
- 差分フレーム転送：変更領域のみエンコード
- アイドルウィンドウはハートビートフレーム。`PNG`/`JPEG` を透過ニーズに応じて自動切替
- 制限超過フレームは JPEG 品質のみ低下——**UI サイズは決して変わらない**（透明ピクセルを含むウィンドウが制限超過時は JPEG へ強制変換し alpha を黒背景に合成。劣化が確実に効く）

### 🎨 シェーダー互換

Iris を自動検出し、バニラのエンティティ描画パイプラインへフォールバック——シェーダー有効時もウィンドウを全輝度で正しく表示します。

---

### 🗺️ 対応プラットフォーム

| プラットフォーム | ローカルウィンドウのキャプチャ | 共有ウィンドウの表示 | ダウンロード |
|------------------|:---:|:---:|---|
| Linux x86_64 | ✅ | ✅ | `waylandcraft-linux-x86_64.jar` |
| Linux arm64 | ✅ | ✅ | `waylandcraft-linux-arm64.jar` |
| Android x86_64（表示のみ） | ❌ | ✅ | `waylandcraft-android-x86_64.jar` |
| Android arm64（表示のみ） | ❌ | ✅ | `waylandcraft-android-arm64.jar` |
| Windows x86_64（表示のみ） | ❌ | ✅ | `waylandcraft-windows-x86_64.jar` |
| Windows arm64（表示のみ） | ❌ | ✅ | `waylandcraft-windows-arm64.jar` |
| macOS x86_64（表示のみ） | ❌ | ✅ | `waylandcraft-macos-x86_64.jar` |
| macOS arm64（表示のみ） | ❌ | ✅ | `waylandcraft-macos-arm64.jar` |
| iOS arm64（表示のみ、実験的） | ❌ | ✅ | `waylandcraft-ios-arm64.jar` |
| 専用サーバー（任意アーキテクチャ） | — | —（give/共有/権限ロジックをホスト） | `waylandcraft-server.jar` |
| ユニバーサル（任意プラットフォーム、救済用） | ✅ | ✅ | `waylandcraft-universal.jar` |

> **プラットフォームごとに専用 jar。** [Releases](https://github.com/scapking/waylandcraft/releases) からお使いのデバイスに合ったファイルを入手してください——各プラットフォーム jar は自分のプラットフォームの native だけを同梱するため、Windows/macOS/iOS は軽量な表示のみ jar（約 0.4 MB）で、プラットフォームを自動検出してローカルキャプチャを無効化します。どれを選べばいいか分からない場合は、全 native プラットフォームを同梱する `waylandcraft-universal.jar` を。専用サーバーは軽量な純 Java の `waylandcraft-server.jar` が使えます。

- **フルモード（Linux）** — キャプチャ・共有・表示が可能。
- **表示のみモード（Android / Windows / macOS）** — 対応プラットフォームの jar をインストールするだけ。プラットフォームを自動検出してローカルキャプチャを無効化し、共有ウィンドウの受信を継続。
- **iOS（表示のみ、実験的）** — [PojavLauncher](https://github.com/PojavLauncherTeam/PojavLauncher_iOS)（後継の [Amethyst](https://github.com/AngelAuraMC/Amethyst-iOS)）で iOS 上に Minecraft Java Edition + Fabric を導入し、`waylandcraft-ios-arm64.jar` をインストールするだけで表示のみモード。未実機検証。
- **サーバー** — 専用サーバーには `waylandcraft-server.jar`（純 Java、native 同梱なし）をインストール。サーバー側の give/共有/権限ロジックは他のすべての jar にも同梱されているため、サーバーにクライアント jar を入れても動作します。
- **ユニバーサル** — `waylandcraft-universal.jar` は全 native プラットフォーム（linux-gnu + android × x86_64/arm64）を同梱。プラットフォームが不明な場合に使います。単一プラットフォーム jar より大きめ（約 6 MB）。

---

## 🚀 クイックスタート

### 前提条件

- Minecraft **26.1.2**（Java Edition）
- Fabric Loader **0.19.x** + Fabric API **0.147.0+26.1.2**
- **Java 25**
- フルモードには Linux **Wayland** セッションが必要（キャプチャに Wayland 必須。X11 のみのセッションは非対応）——Windows/macOS/Android は表示のみモード（[対応プラットフォーム](#️-対応プラットフォーム)参照）

### インストール

1. Fabric Loader と Fabric API をインストール。
2. [Releases](https://github.com/scapking/waylandcraft/releases) からお使いのプラットフォーム/アーキテクチャに合った jar（[対応プラットフォーム](#️-対応プラットフォーム)参照）を `.minecraft/mods/` に入れる——デスクトップ Linux は `waylandcraft-linux-x86_64.jar`、スマホは `waylandcraft-android-arm64.jar`、Apple Silicon は `waylandcraft-macos-arm64.jar`。
3. **マルチプレイ：サーバーにも mod が必要**（give/権限/共有ロジックはサーバー側。無い場合はこれらの機能が静かに無効化される）。
4. ゲームを起動——シングルプレイの世界はサーバーを内蔵し、同じ `mods/` フォルダを共有。

### 最初の一歩

```text
/wl launch firefox              # アプリ起動（または V キー）
/wl list windows                # ウィンドウ一覧。行末の 4 桁ランダムコードがエイリアス
/wl give <handle>               # ウィンドウをアイテム化。右クリック長押しで設置
/wl grab <handle>               # ウィンドウを掴んでドラッグ（G キーでキーボードキャプチャ切替）
/wl share start <handle>        # ウィンドウをチームメイトへ共有
```

> 💡 **スマホで見る？** `waylandcraft-android-<arch>.jar`（多くのスマホは arm64）を入れてサーバーに参加するだけ——共有ウィンドウが自動で表示されます。

---

## 📖 使い方ガイド

### ウィンドウ管理

| 操作 | コマンド |
|------|----------|
| 起動可能アプリ一覧 | `/wl list` · `/wl list apps` |
| ウィンドウ一覧 | `/wl list windows` |
| デスクトップウィンドウをキャプチャ | `/wl capture` |
| アプリ起動 | `/wl launch <app>` |
| ウィンドウをアイテム化 | `/wl give <handle>` |
| ウィンドウを取り戻す | `/wl take <handle>` |
| ウィンドウを掴む/ドラッグ | `/wl grab <handle>` |
| 世界内で表示/非表示 | `/wl show <handle|all>` / `/wl hide <handle|all>` |
| 固定（常時表示） | `/wl pin <handle>` / `/wl unpin <handle>` |
| アプリ終了 | `/wl close <handle>` |
| サイズ変更 | `/wl resize <handle> <w> <h>` |
| 位置確認 | `/wl pos <handle>` |
| 移動（絶対値または `~` 相対値） | `/wl move <handle> <x> <y> <z>` |
| 回転（角度） | `/wl rotate <handle> <angle>` |
| X11 デスクトップウィンドウ一覧 | `/wl x11 list` |
| X11 ウィンドウを直接共有 | `/wl x11 share <index>` |
| X11 共有を停止 | `/wl x11 stop <handle>` |

**ハンドル形式** — `<handle>` は：`0x` 短ハンドル、完全ハンドル、**インスタンスエイリアス**（4 桁ランダム、例 `k7xq`、`/wl list windows` 由来、セッション内で一意）、アプリエイリアス（例 `firefox_esr`）に対応。同一アプリの複数ウィンドウは `エイリアス:N`（例 `firefox:2`）。

### レイアウトテンプレート

| コマンド | 用途 |
|----------|------|
| `/wl template save <name>` | 現在のレイアウトを保存（一時、再起動で消える） |
| `/wl template savep <name>` | 永続テンプレートを保存（アプリ+位置+解像度、ディスク保存） |
| `/wl template apply <name>` | 一時テンプレートを復元 |
| `/wl template applyp <name>` | 永続テンプレート適用：アプリを自動起動して配置 |
| `/wl template list` | 全テンプレート一覧 |
| `/wl template remove <name>` / `removep <name>` | 一時/永続テンプレートを削除 |

### 自動レイアウト（cube / sphere）

ウィンドウを固定された初期化原点を中心に自動配置できる（プレイヤーに追従しなくなる）。デフォルトでは無効。

| コマンド | 用途 |
|----------|------|
| `/wl layout init [<x> <y> <z> [<yaw>]]` | レイアウト中心 + ヨーを初期化（引数なし = プレイヤー位置） |
| `/wl layout cube` | cube テンプレートに切り替え（4 面 × 各面 N ウィンドウ）して有効化 |
| `/wl layout sphere` | sphere テンプレートに切り替え（VR スクリーン壁リング、上に積み上げ）して有効化 |
| `/wl layout on` / `off` / `toggle` | 自動レイアウトを有効/無効/切り替え |
| `/wl layout status` | テンプレート・中心・半径・間隔・コアウィンドウを表示 |
| `/wl layout list` | レイアウト内のウィンドウ一覧（`➤` はコアウィンドウ） |
| `/wl layout add <handle>` / `remove <handle>` | 手動でウィンドウを追加/削除（`layoutAutoJoin` が off のとき） |
| `/wl layout core <handle>` | コアウィンドウを明示指定 |

* `Ctrl` + 矢印キーで**コアマーカー**をその方向の隣のウィンドウへ移動——任意のウィンドウがコアになれる（左右は折り返し、上下はレイヤー間移動、無制限）。コアウィンドウはワールド内でシアンの輪郭でハイライト。自動レイアウト無効時も、`Ctrl` + 矢印キーでホバー中のウィンドウを手動移動できる。
* `G` キーでキーボードをキャプチャ；デフォルトの `H` キーでカーソルを切り替え（両方ともバニラのキー設定で再バインド可能）。

### 共有

| コマンド | 用途 |
|----------|------|
| `/wl share start <handle>` | 共有を開始（`all` / `*` = 全ウィンドウを一括共有） |
| `/wl share stop <handle>` | 共有を停止（`all` / `*` = すべて停止） |
| `/wl share quality <handle> <s> <q> <fps>` | スケール/品質/フレームレートを設定 |
| `/wl share preset <handle> <preset>` | プリセット適用（[設定](#️-設定)参照） |
| `/wl share config <handle> <param> <value>` | 単一パラメータ調整 |
| `/wl share reset <handle>` | デフォルトに戻す |
| `/wl share info <handle>` | 現在の共有設定を表示 |
| `/wl share resolution <handle> <w> <h>` | ターゲット解像度を設定 |
| `/wl share stats <handle>` | 共有統計を表示 |

### 権限

| コマンド | 用途 |
|----------|------|
| `/wl permission list` | 全権限を一覧 |
| `/wl permission default <PERM>` | デフォルト権限を設定 |
| `/wl permission allow <player> <PERM>` | プレイヤーをホワイトリスト登録 |
| `/wl permission deny <player>` | プレイヤーをブラックリスト登録 |
| `/wl permission remove <player>` | プレイヤーを削除 |

`PERM`：`NONE` / `VIEW` / `INTERACT` / `CONTROL`

---

## 📚 コマンドリファレンス

### 設定

| コマンド | 用途 |
|----------|------|
| `/wl settings list` | 現在の設定を表示 |
| `/wl settings set <key> <value>` | 設定を変更 |

### 共有画質パラメータ

`/wl share config <handle> <パラメータ> <値>` で設定可能：

| パラメータ | 説明 | 範囲 |
|------------|------|------|
| `scale` | 解像度スケール | 0.1 – 1.0 |
| `quality` | JPEG 品質 | 0.1 – 1.0 |
| `fps` | 最大フレームレート | 5 – 120 |
| `bitrate` | 最大ビットレート（kbps） | 0 = 無制限 |
| `diffThreshold` | ピクセル変化閾値 | 0.001 – 1.0 |
| `diff` | 差分フレーム転送のオン/オフ | true / false |
| `buffer` | フレームバッファ数 | 1 – 8 |
| `latency` | 遅延補正（ms） | 0 – 500 |
| `prediction` | モーション予測のオン/オフ | true / false |
| `compression` | 圧縮方式 | 例 `lz4` / `zlib` / `none` |

### プリセット

| プリセット | スケール | 品質 | FPS | ビットレート |
|------------|----------|------|-----|--------------|
| `performance` | 0.25 | 0.5 | 60 | 1000 kbps |
| `balanced` | 0.5 | 0.7 | 30 | 2000 kbps |
| `quality` | 1.0 | 1.0 | 30 | 無制限 |
| `lowlatency` | 0.35 | 0.6 | 60 | 1500 kbps |

### X11 ウィンドウ共有

Wayland トップレベルを経由せず、X11 デスクトップのウィンドウを直接共有（`xwayland-satellite` 経由）：

| コマンド | 用途 |
|----------|------|
| `/wl x11 list [display]` | X11 デスクトップウィンドウを一覧（デフォルトは satellite ディスプレイ） |
| `/wl x11 share <index>` | 一覧の N 番目のウィンドウを共有 |
| `/wl x11 stop <handle>` | X11 ウィンドウの共有を停止 |

### グローバル設定

`/wl settings set <key> <value>` は以下の全キーに対応（`/wl settings list` でも確認可）：

| キー | デフォルト | 説明 |
|------|------------|------|
| `pixelsPerBlock` | `500` | 1 ブロックあたりのウィンドウ画素密度 |
| `windowAntialiasing` | `false` | RGSS アンチエイリアス（シェーダーなし時のみ） |
| `focusOnHover` | `false` | ホバーで自動フォーカス |
| `hideCursor` | `false` | ウィンドウ操作中に仮想マウスカーソルを非表示 |
| `layoutEnabled` | `true` | レイアウトをデフォルトで有効化（v0.2.37 の動作；未初期化時はプレイヤー位置で自動初期化） |
| `layoutAutoJoin` | `true` | 新規ウィンドウを自動でレイアウトに参加（false = `/wl layout add` で指定したもののみ） |
| `layoutTemplate` | `cube` | レイアウトテンプレート：`cube` または `sphere` |
| `layoutInitialized` | `false` | `/wl layout init` 実行済みか（未初期化ならレイアウト不可） |
| `layoutInitX` / `Y` / `Z` | `0.0` | レイアウト中心座標 |
| `layoutInitYaw` | `0.0` | レイアウト向き（度、0=+Z 方向、時計回り） |
| `layoutRadius` | `6.0` | レイアウト半径（ブロック、中心からウィンドウまでの水平距離） |
| `layoutSpacing` | `0.4` | 同一レイヤーのウィンドウ間の最小水平間隔（ブロック） |
| `layoutStackSpacing` | `0.4` | レイヤー間の垂直間隔（ブロック） |
| `layoutCubePerFace` | `2` | cube テンプレートの各面あたりウィンドウ数（4 面で計 8） |
| `layoutDefaultWidth` | `1080` | レイアウト参加時に適用する解像度 |
| `layoutDefaultHeight` | `540` | レイアウト参加時に適用する解像度 |
| `groundClearance` | `0.4` | ウィンドウ下端の地面からの最小クリアランス（ブロック） |
| `moveStep` | `0.5` | Ctrl+矢印キーでの手動移動ステップ |

---

## 🎨 シェーダー互換性

- Iris シェーダー有効時、ウィンドウは**バニラのエンティティ描画パイプライン**で全輝度描画——シェーダーの照明の影響を受けません。
- シェーダーなし時はカスタムパイプラインを使用。RGSS アンチエイリアス（`windowAntialiasing`）も選択可。
- どちらのモードでもウィンドウの前面にテクスチャ、**背面は純黒**——挙動は完全に同一。

---

## 💡 ヒントとベストプラクティス

- **ウィンドウは常に垂直** — ドラッグ中は高さ軸がロックされ、下端は地面から **0.4 ブロック** より下に下がりません。`Ctrl+ホイール` で向きを回転（垂直は維持）。
- **正確な配置** — `/wl pos <handle>` で現在の姿勢を読み取り、`/wl move` で正確な座標を設定（`~` 相対オフセット対応）、`/wl rotate` で向きを設定。
- **サーバーにも mod 必須** — マルチプレイでは `give` / `permission` / `share` がサーバー側ロジックに依存。サーバーに mod が無いとリクエストは静かに破棄されます。
- **角丸・影のわずかなジャギーは正常** — JPEG 圧縮によるもの。透明ウィンドウは自動で PNG になり alpha を保持。
- **ゲーム内の完全ヘルプ**：`/wl help`。

---

## ❓ FAQ

**Q: サーバーにも mod が必要なのはなぜ？**
A: `give`、権限、共有ロジックはサーバー側に登録されています。サーバーに mod が無いと、これらの機能は静かに無効化されます。

**Q: スマホで共有ウィンドウを見られますか？**
A: 見られます。`waylandcraft-android-<arch>.jar` を入れてサーバーに参加してください。PC 側が共有したウィンドウが自動で表示されます。

**Q: X11 アプリは動きますか？**
A: 動きます。`xwayland-satellite` が jar に同梱されており（`x86_64`/`arm64` 両対応）、X11 アプリに `DISPLAY` が自動提供されます。システムに `Xwayland` が必要ですが、ほぼすべての Wayland デスクトップに付属しています。

**Q: Windows / macOS は対応していますか？**
A: 表示のみモードで対応。同じ `waylandcraft.jar` をインストールするだけ——プラットフォームを自動検出してローカルキャプチャを無効化し、共有ウィンドウの受信は継続します。iOS は PojavLauncher/Amethyst で Java Edition + Fabric を実行して対応（実験的、未実機検証）。

**Q: 共有画面がぼやける/遅い。どう改善する？**
A: 品質かフレームレートを上げてください：`/wl share quality <handle> <スケール> <品質> <fps>`、または `quality` プリセットを適用。デフォルトはバランス設定です。画質を下げても UI サイズは変わりません。

**Q: シェーダー有効時にウィンドウが透明/黒くなる。**
A: Iris を自動検出しバニラパイプラインへフォールバックします。全クライアントとサーバーが同じバージョン（≥ v0.2.32）であることを確認してください。

**Q: 上流の WaylandCraft との違いは？**
A: このフォークは、マルチプレイヤーでのウィンドウ共有、権限システム、純 CLI モード、モバイル視聴、および上記のパフォーマンス/画質設計を（AI 支援で）追加実装しています。

---

## 🚧 既知の制限

1. **ウィンドウ移動は制御モード** — ウィンドウは垂直配置に固定され、ドラッグ中は高さ軸がロックされます（下端が地面から 0.4 ブロック以上を保証）。意図的な簡略化であり、より自由な配置は将来の拡張で検討します。
2. **共有画質と遅延のトレードオフ** — 共有元と UI サイズを一致させるため、制限超過時は JPEG 品質のみ低下させ、解像度は下げません。高解像度ウィンドウは弱いサーバー/スマホで転送・デコード負荷が残ります。

---

## 🏗️ ソースからのビルド

```bash
# 前提: Java 25、Rust ツールチェーン、Wayland 開発ライブラリ
apt install libwayland-dev libxkbcommon-dev pkg-config libclang-dev

# 1. Rust ネイティブライブラリをビルド（release 必須。build.gradle は release .so を優先）
source ~/.cargo/env
cd native && cargo build --release

# 2. Java mod をビルド
cd .. && ./gradlew clean build

# 出力: build/libs/waylandcraft.jar（約 6.0MB、x86_64 と arm64 の xwayland-satellite を同梱）
```

> ⚠️ パッケージされるネイティブライブラリが `native/target/release/libwaylandcraft.so`（約 3.7MB）であることを確認してください。debug ビルド（デバッグシンボル付き 176MB）を誤ってパッケージすると jar が 39MB に膨らみます。

> 📦 同梱の `xwayland-satellite` バイナリは `native/build-satellite.sh` でビルドします——`x86_64` と `arm64` それぞれ 1 回実行し（arm64 は `aarch64-linux-gnu-gcc` + `cargo build --target aarch64-unknown-linux-gnu` でクロスコンパイル）、両バイナリを `native/` に配置してください。

---

## 🧱 アーキテクチャ

```text
┌─────────────────────────────────────────────────────────────┐
│                      Minecraft クライアント                  │
│  ┌──────────────┐   ┌───────────────────┐   ┌────────────┐  │
│  │  ウィンドウ表示 │   │  WindowShare      │   │  /wl CLI   │  │
│  │  (描画)      │◄─►│  (キャプチャ/送信) │◄─►│  (コマンド) │  │
│  └──────┬───────┘   └────────┬──────────┘   └────────────┘  │
│         │  PBO/GPU リードバック │ Fabric Networking API      │
└─────────┼────────────────────┼──────────────────────────────┘
          │                    │
┌─────────┼────────────────────┼──────────────────────────────┐
│         ▼                    ▼            Minecraft サーバー │
│  ┌─────────────────────────────────────────────┐            │
│  │  SharedWindowManager（権限・状態）            │            │
│  └──────────────────────┬──────────────────────┘            │
│                         │ フレーム                           │
│  ┌──────────────────────▼──────────────────────┐            │
│  │  SharedWindowFrameRelay（ワーカースレッド分散）│            │
│  └──────────────────────┬──────────────────────┘            │
└─────────────────────────┼────────────────────────────────────┘
                          │ ブロードキャスト
          ┌───────────────▼────────────────┐
          │  視聴端（PC または Android）    │
          │  非同期デコード → 世界内描画    │
          └────────────────────────────────┘
```

| レイヤー | 技術 |
|----------|------|
| ゲーム | Java 25, Fabric Loader 0.19.2+, Fabric API 0.147.0+ |
| ネイティブブリッジ | Rust, JNI |
| Wayland | Smithay, wayland-client, wlr-foreign-toplevel-management |
| 画像 | PBO 非同期リードバック, glBlitFramebuffer, JPEG/PNG, MemoryUtil |
| ネットワーク | Fabric Networking API, カスタム Payload プロトコル |

---

## 🤝 コントリビュート

コントリビューション歓迎です！これは [WaylandCraft](https://github.com/EVV1E/waylandcraft.git) のフォークで、マルチプレイ機能は AI によって実装されています——荒削りな部分があります。

- バグ報告・機能要望は [GitHub Issues](../../issues) へ
- 修正・改善は [Pull Requests](../../pulls) へ
- Java と Rust の両方をビルド可能に保つこと：`native/` で `cargo build --release`、リポジトリルートで `./gradlew build`
- ユーザー向け挙動を変更したら README（EN/ZH/JA）も更新すること

---

## 📜 変更履歴

完全な履歴は [Releases](https://github.com/scapking/waylandcraft/releases) ページをご覧ください。

**最近のハイライト：**

- **v0.9.16** — `/wl show all` / `/wl hide all`（`*` も可）で全ウィンドウを一括表示/非表示；`hide all` はピン留めも解除。
- **v0.9.15** — ウィンドウの開閉に応じてレイアウト順序をリアルタイム更新：番号を詰め直して隙間を埋める。
- **v0.9.14** — Ctrl+矢印を隣接ウィンドウとの位置交換（swapCore）に変更し、左右方向の反転も修正。
- **v0.9.13** — Ctrl+矢印をホバー中のウィンドウ移動に復元（manualOffset が毎フレームの再配置に上書きされないように）。
- **v0.9.12** — `/wl share start all` で全ウィンドウを一括共有（`stop all` も同様）；Ctrl+矢印でレイアウトのコアマーカーを切替。
- **v0.9.11** — キーボード透過の根本原因を修正：`xkb_state.update_key` に evdev キーコード（`key-8`）を渡していたが、xkbcommon は xkb キーコード（evdev+8）を要求する。無効なキーコードは黙って無視され、Ctrl/Shift/Alt の修飾ビットが永遠に立たない——単独キーは正常に見えるのに、ショートカット（Ctrl+L など）は全て効かない状態だった。kb.log で「Ctrl を押しているのに mods(depressed=0)」が証拠。**修飾キー付きの組合せキーが正常に透過するようになった。**
- **v0.9.10** — `setKbLogFileNative` の JNI 登録名不一致を修正（Rust マクロの snake→camel 自動生成 vs 明示名）。
- **v0.9.9** — Rust キーボードログを独立ファイル `waylandcraft-kb.log`（setKbLogFile）に書き出し。Rust 側のフォーカス/送信状態の診断が容易に。
- **v0.9.8** — キーボード透過の主因を修正：`correctScancode` が Wayland で +8 しなくなった（キーコード二重オフセット）。
- **v0.9.7** — ログを整理；`keyboard_key` が修飾キー状態を毎キー出力；tick フォーカスログをスロットル化；`keyboard_focus` を冪等サイレント化。
- **v0.9.6** — キーボード透過の全パイプライン debug ログ（mixin 入口/onKeyPress 分岐/ローカル転送/bridge/Rust フォーカス/毎キー送信）。
- **v0.9.5** — ローカルウィンドウのキーボード透過（シナリオ B）を修正：フォーカスフォールバック + 転送自己復旧 + 診断ログ。
- **v0.9.4** — Ctrl+矢印キーの方向反転と、G バインド中の J キー誤発動を修正。
- **v0.9.3** — 共有ウィンドウの長押し REPEAT 透過を補完（`forwardSharedKey` の Repeat 分岐、要件 1 を完成）。
- **v0.9.2** — Ctrl+矢印キーをレイアウト順序の入れ替え（プラン A）に変更——範囲制限なし。
- **v0.9.1** — G バインド後に全キーが効かなくなる問題を修正——バインド/ホバー時にキーボードフォーカスを設定（focusSurface）。
- **v0.9.0** — キーボード入力サブシステムを再構築（プラン C）：長押し REPEAT イベントを完全透過（長押し不能を修正）；組合せキー/大文字小文字は Rust xkb ステートマシンが一元的に管理、Java は透過のみ；Ctrl+矢印キーで**常にウィンドウを移動**（v0.2.37 の意味論を復元、レイアウトコア切り替えは解除）；キャプチャ分担 **G=キーボードのみ、J=キーボード+マウス**；release がバージョン変更内容に基づき自動生成。
- **v0.2.35** — iOS 検出を追加（PojavLauncher/Amethyst ランタイム）：表示のみモード、同じ jar、共有ウィンドウの表示は継続。対応プラットフォーム表を更新。
- **v0.2.34** — Windows/macOS を**表示のみモード**で対応：プラットフォームを自動検出してローカルキャプチャをスキップ。同じ jar が Linux/Windows/macOS/Android で動作し、共有ウィンドウの表示は継続。
- **v0.2.33** — ウィンドウのインスタンスエイリアスを 4 桁のランダムコード（例 `k7xq`）に変更（w1/w2… は廃止）。紛らわしい `0/o/1/l/i` を除外し入力しやすく。
- **v0.2.32** — 透明ウィンドウの JPEG 強制劣化（品質調整が実際に有効に）；単一フレーム上限を 600 KB → 1.8 MB に引き上げ。
- **v0.2.31** — サーバーのフレーム中継をウィンドウ単位で N スレッドに分散（同一ウィンドウは順序維持、別ウィンドウは並列）；登録/登録解除を netty スレッドから分離。
- **v0.2.30** — フレーム転送を Server スレッドから完全に分離；PBO を恒久フォールバック化；デフォルト q0.85 / 10 fps。

---

## 📄 ライセンス

MIT License — 詳細は [LICENSE](LICENSE) をご覧ください。

## 謝辞

- [WaylandCraft](https://github.com/EVV1E/waylandcraft.git) — オリジナルプロジェクト
- [Smithay](https://github.com/Smithay/smithay) — Wayland コンポジターフレームワーク
- [Fabric](https://fabricmc.net/) — Minecraft モッドローダー
