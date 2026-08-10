#!/usr/bin/env python3
"""Verify a waylandcraft jar contains exactly the expected content.

Usage: verify_jar.py <platform> [arch]
  platform: linux-gnu | android | windows | macos | ios | server | universal
  arch:     x86_64 | arm64   (required for single-platform jars)

- linux-gnu/android: the arch's native lib + bundled deps (linux also satellite)
- windows/macos/ios: viewer-only, no native payload at all
- server:            no native payload at all (pure Java server logic)
- universal:         all native platforms (linux-gnu + android x86_64/arm64),
                     both linux satellites, all four deps bundles
"""
import glob
import sys
import zipfile

platform = sys.argv[1]
arch = sys.argv[2] if len(sys.argv) > 2 else ''

VIEWER_PLATFORMS = ('windows', 'macos', 'ios', 'server')
UNIVERSAL = platform == 'universal'

jar_platform = 'linux' if platform == 'linux-gnu' else platform
pattern = f"build/libs/waylandcraft-{jar_platform}{'-' + arch if arch else ''}.jar"
jars = glob.glob(pattern)
assert jars, f"jar not found for {pattern}"
jar = jars[0]

z = zipfile.ZipFile(jar)
names = z.namelist()
libs = [n for n in names if n.startswith("libwaylandcraft-")]
sats = [n for n in names if n.startswith("xwayland-satellite-")]
deps = [n for n in names if n.startswith("native-deps/")]

print("jar:", jar)
print("native libs:", libs)
print("satellites:", sats)
print("bundled deps:", sorted(set(n.split('/')[1] for n in deps if '/' in n)))

if platform in VIEWER_PLATFORMS:
    # Viewer-only / server jar: no native payload whatsoever.
    assert libs == [], f"viewer/server jar should not bundle native libs: {libs}"
    assert sats == [], f"viewer/server jar should not bundle satellites: {sats}"
    assert deps == [], f"viewer/server jar should not bundle deps: {deps}"
    print("OK (no native payload)")
    sys.exit(0)

if UNIVERSAL:
    expected_libs = {
        "libwaylandcraft-linux-gnu-x86_64.so",
        "libwaylandcraft-linux-gnu-arm64.so",
        "libwaylandcraft-android-x86_64.so",
        "libwaylandcraft-android-arm64.so",
    }
    assert set(libs) == expected_libs, f"unexpected native libs: {libs}"
    expected_sats = {
        "xwayland-satellite-linux-gnu-x86_64",
        "xwayland-satellite-linux-gnu-arm64",
    }
    assert set(sats) == expected_sats, f"unexpected satellites: {sats}"
    for t in ("linux-gnu-x86_64", "linux-gnu-arm64", "android-x86_64", "android-arm64"):
        assert f"native-deps/{t}/deps.list" in names, f"deps.list missing for {t}"
    print("OK (universal: 4 native + 2 satellite + 4 deps)")
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
