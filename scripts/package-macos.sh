#!/usr/bin/env bash
# Build the macOS release artifact: Supersprinter.app inside a drag-and-drop
# DMG (supersprinter-v<VERSION>-macos-<ARCH>.dmg).
#
# Unsigned: no Developer ID in scope, so Gatekeeper shows a warning on first
# launch — right-click > Open once, documented in README. ffmpeg is NOT
# bundled: video export requires `brew install ffmpeg` at runtime.
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: scripts/package-macos.sh --version VERSION [--arch arm64|x86_64]
                                [--no-build] [--skip-dmg] [--out-dir DIR]

Defaults: native arch, release build if needed, output to dist/release.
EOF
}

version=""
arch=""
build=1
create_dmg=1
out_dir=""

while [[ $# -gt 0 ]]; do
  case "$1" in
    --version) version="${2:-}"; shift 2 ;;
    --arch) arch="${2:-}"; shift 2 ;;
    --no-build) build=0; shift ;;
    --skip-dmg) create_dmg=0; shift ;;
    --out-dir) out_dir="${2:-}"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown arg: $1" >&2; usage >&2; exit 2 ;;
  esac
done

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
if [[ -z "$version" ]]; then
  version="$(cargo metadata --format-version 1 --no-deps \
    --manifest-path "${repo_root}/Cargo.toml" \
    | jq -r '.packages[] | select(.name=="supersprinter-gui").version')"
fi
[[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-rc\.[0-9]+)?$ ]] || {
  echo "Invalid --version '$version'. Expected 0.1.0 or 0.1.0-rc.1." >&2
  exit 2
}
asset_version="${version//\~/.}"

host_arch_raw="$(uname -m)"
case "$host_arch_raw" in
  arm64|aarch64) host_arch="arm64" ;;
  x86_64|amd64) host_arch="x86_64" ;;
  *) echo "Unsupported machine architecture: $host_arch_raw" >&2; exit 1 ;;
esac
arch="${arch:-$host_arch}"
if [[ "$arch" != "arm64" && "$arch" != "x86_64" ]]; then
  echo "Unsupported --arch '$arch'. Expected arm64 or x86_64." >&2
  exit 2
fi
if [[ "$arch" != "$host_arch" ]]; then
  echo "Requested --arch '$arch' does not match host '$arch'. Use a native runner." >&2
  exit 1
fi

bin_src="${repo_root}/target/release/supersprinter-gui"
if [[ $build -eq 1 && ! -x "$bin_src" ]]; then
  (cd "$repo_root" && cargo build --release --locked --bin supersprinter-gui)
fi
if [[ ! -x "$bin_src" ]]; then
  echo "Binary not found: $bin_src — build first or omit --no-build." >&2
  exit 1
fi

if [[ -z "$out_dir" ]]; then
  out_dir="${repo_root}/dist/release"
elif [[ "$out_dir" != /* ]]; then
  out_dir="${repo_root}/${out_dir}"
fi
mkdir -p "$out_dir"

release_root="supersprinter-v${asset_version}-macos-${arch}"
stage_root="${out_dir}/stage"
release_dir="${stage_root}/${release_root}"
app_bundle="${release_dir}/Supersprinter.app"
contents_dir="${app_bundle}/Contents"
macos_dir="${contents_dir}/MacOS"
resources_dir="${contents_dir}/Resources"

rm -rf "$release_dir"
mkdir -p "$macos_dir" "$resources_dir"

# The crate bin is supersprinter-gui; inside the bundle it is `supersprinter`.
install -m755 "$bin_src" "${macos_dir}/supersprinter"
install -m644 "${repo_root}/README.md" "${resources_dir}/README.md"
install -m644 "${repo_root}/LICENSE" "${resources_dir}/LICENSE"

if [[ ! -f "${repo_root}/assets/supersprinter.icns" ]]; then
  echo "Missing ${repo_root}/assets/supersprinter.icns — run scripts/gen-icons.sh." >&2
  exit 1
fi
install -m644 "${repo_root}/assets/supersprinter.icns" "${resources_dir}/AppIcon.icns"

cat > "${contents_dir}/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "https://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>en</string>
  <key>CFBundleDisplayName</key>
  <string>Supersprinter</string>
  <key>CFBundleExecutable</key>
  <string>supersprinter</string>
  <key>CFBundleIconFile</key>
  <string>AppIcon</string>
  <key>CFBundleIdentifier</key>
  <string>dev.piotreknow02.supersprinter</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>Supersprinter</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>${version}</string>
  <key>CFBundleVersion</key>
  <string>${version}</string>
  <key>LSMinimumSystemVersion</key>
  <string>11.0</string>
  <key>NSHighResolutionCapable</key>
  <true/>
</dict>
</plist>
PLIST

dmg_path="${out_dir}/${release_root}.dmg"
if [[ $create_dmg -eq 1 ]]; then
  command -v hdiutil >/dev/null 2>&1 || { echo "hdiutil not found (macOS only)." >&2; exit 1; }
  dmg_stage="${out_dir}/dmg-stage-${arch}"
  rm -rf "$dmg_stage"
  mkdir -p "$dmg_stage"
  cp -R "$app_bundle" "${dmg_stage}/Supersprinter.app"
  ln -s /Applications "${dmg_stage}/Applications"

  # hdiutil's inferred size can be too small; add 30% plus 64 MiB slack.
  source_kib="$(du -sk "$dmg_stage" | awk '{print $1}')"
  dmg_size_mib=$(( (source_kib * 13 / 10 + 65536 + 1023) / 1024 ))
  echo "Creating DMG with ${dmg_size_mib} MiB filesystem."
  rm -f "$dmg_path"
  hdiutil create \
    -volname "Supersprinter" \
    -srcfolder "$dmg_stage" \
    -fs HFS+ \
    -size "${dmg_size_mib}m" \
    -ov \
    -format UDZO \
    "$dmg_path" >/dev/null
  rm -rf "$dmg_stage"
fi

echo "Packaged macOS artifacts:"
if [[ $create_dmg -eq 1 ]]; then
  echo "  $dmg_path"
else
  echo "  ${app_bundle} (DMG skipped via --skip-dmg)"
fi
