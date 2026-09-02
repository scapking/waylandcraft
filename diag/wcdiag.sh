#!/usr/bin/env bash
# WaylandCraft mod 一键诊断日志打包
# Usage: bash wcdiag.sh
# Output: /tmp/wcdiag-<timestamp>.tar.gz + 上传 URL
#
# v0.13.11 (mod 1.2.15) 改进：
# - 加 cp mods/waylandcraft*.jar  (v1.2.13 缺失)
# - 加 cp waylandcraft/satellite.log  (v0.13.10 satellite 子目录)
# - 加系统诊断 (wayland-scanner / Xwayland / java / glxinfo)
# - 上传 catbox 失败自动 fallback tmpfiles

set -e
D=/tmp/wcdiag-$(date +%s)
mkdir -p "$D"
cd ~/minercaft_26_1_2 2>/dev/null || cd ~/.minecraft

# 1. 主日志
cp waylandcraft-ime.log waylandcraft-audio.log waylandcraft-kb.log "$D/" 2>/dev/null
tail -c 3000000 logs/latest.log > "$D/latest.log.tail" 2>/dev/null

# 2. 崩溃报告 + hs_err
ls crash-reports 2>/dev/null | tail -5 | while read -r f; do
    cp "crash-reports/$f" "$D/" 2>/dev/null
done
cp hs_err_pid*.log "$D/" 2>/dev/null

# 3. mod 配置目录（v1.2.9+ 把所有日志都放这里）
cp -r waylandcraft "$D/wc-dir" 2>/dev/null

# 4. status.log（v1.2.9+ 覆盖式 status 报告）
cp waylandcraft/status.log "$D/status.log" 2>/dev/null

# 5. satellite log（v0.13.10+ 也可能写到 waylandcraft 子目录）
cp waylandcraft/satellite.log "$D/wc-satellite.log" 2>/dev/null

# 6. 临时文件位置（不一定在 gameDir）
cp /tmp/wlc-env-dump.log /tmp/waylandcraft-app-bash.log /tmp/waylandcraft-launch.log /tmp/waylandcraft-satellite.log "$D/" 2>/dev/null

# 7. mod jar（v0.13.11 改进：原脚本漏拷——没法直接看 version/date）
cp mods/waylandcraft*.jar "$D/" 2>/dev/null

# 8. 环境 + 系统诊断
{
    echo "== session =="
    echo "XDG_SESSION_TYPE=$XDG_SESSION_TYPE WAYLAND_DISPLAY=$WAYLAND_DISPLAY DISPLAY=$DISPLAY XDG_CURRENT_DESKTOP=$XDG_CURRENT_DESKTOP"
    echo "== ime processes =="
    pgrep -a fcitx5 2>/dev/null
    pgrep -a ibus 2>/dev/null
    echo "== fcitx5 journal =="
    journalctl --user -u fcitx5 --no-pager 2>&1 | tail -200
    echo "== mods =="
    ls -la mods/ | grep -iE 'wayland|ime'
    echo "== wc version in jar =="
    unzip -p mods/waylandcraft*.jar fabric.mod.json 2>/dev/null | head -8
    echo "== wayland version =="
    wayland-scanner --version 2>/dev/null || echo "wayland-scanner not installed"
    echo "== xwayland version =="
    Xwayland -version 2>/dev/null || echo "Xwayland not installed"
    echo "== java version =="
    java -version 2>&1
    echo "== uname =="
    uname -a
    echo "== mem =="
    free -h | head -3
    echo "== disk =="
    df -h "$HOME" 2>/dev/null | head -3
    echo "== gpu =="
    lspci 2>/dev/null | grep -iE 'vga|3d' | head -3
    echo "== glx =="
    glxinfo 2>/dev/null | head -5 || echo "glxinfo not installed"
} > "$D/env.txt" 2>&1

# 9. 打包
cd /tmp
TARFILE="wcdiag-$(date +%s).tar.gz"
tar czf "$TARFILE" "$(basename "$D")"

# 10. 上传（catbox 失败 fallback tmpfiles）
UPLOAD_URL=$(curl -sf -F reqtype=fileupload -F time=72h -F fileToUpload=@"$TARFILE" \
    https://litterbox.catbox.moe/resources/internals/api.php 2>/dev/null)

if [ -z "$UPLOAD_URL" ]; then
    UPLOAD_URL=$(curl -sf -F "file=@$TARFILE" https://tmpfiles.org/api/v1/upload 2>/dev/null | \
        python3 -c "import sys,json; print(json.load(sys.stdin).get('data',{}).get('url',''))" 2>/dev/null)
fi

if [ -n "$UPLOAD_URL" ]; then
    echo "Uploaded: $UPLOAD_URL"
fi
echo "Local copy: /tmp/$TARFILE"
