#!/usr/bin/env python3
"""Verify a single-platform waylandcraft jar contains exactly the expected
content: the arch's native lib + (for linux) satellite + bundled deps for
native platforms; no native payload at all for viewer-only platforms.

Usage: verify_jar.py <platform> <arch>
  platform: linux-gnu | android | windows | macos | ios
  arch:     x86_64 | arm64
"""
import glob
import sys
import zipfile

platform = sys.argv[1]
arch = sys.argv[2]

VIEWER_PLATFORMS = ('windows', 'macos', 'ios')

jar_platform = 'linux' if platform == 'linux-gnu' else platform
jars = glob.glob(f"build/libs/waylandcraft-{jar_platform}-{arch}.jar")
assert jars, f"jar not found for {jar_platform}-{arch}"
jar = jars[0]

z = zipfile.ZipFile(jar)
names = z.namelist()
libs = [n for n in names if n.startswith(f"libwaylandcraft-{platform}-")]
sats = [n for n in names if n.startswith("xwayland-satellite-")]
deps = [n for n in names if n.startswith(f"native-deps/{platform}-{arch}/")]

print("jar:", jar)
print("native libs:", libs)
print("satellites:", sats)
print("bundled deps:", deps)

if platform in VIEWER_PLATFORMS:
    # Viewer-only jar: no native payload whatsoever.
    assert libs == [], f"viewer jar should not bundle native libs: {libs}"
    assert sats == [], f"viewer jar should not bundle satellites: {sats}"
    assert deps == [], f"viewer jar should not bundle deps: {deps}"
    assert not any(n.startswith("libwaylandcraft-") for n in names), \
        f"viewer jar should not bundle any native lib: {[n for n in names if n.startswith('libwaylandcraft-')]}"
    print("OK (viewer-only)")
    sys.exit(0)

expected_lib = f"libwaylandcraft-{platform}-{arch}.so"
assert libs == [expected_lib], f"unexpected native libs: {libs}"

if platform == 'linux-gnu':
    expected_sat = f"xwayland-satellite-linux-gnu-{arch}"
    assert sats == [expected_sat], f"unexpected satellites: {sats}"
else:
    assert sats == [], f"android jar should not bundle xwayland-satellite: {sats}"

# Android jars must bundle the bionic deps + manifest; linux jars too
# (collected by collect_deps.py in CI).
assert deps, f"no bundled native deps under native-deps/{platform}-{arch}/"
assert f"native-deps/{platform}-{arch}/deps.list" in names, "deps.list manifest missing"
print("OK")
