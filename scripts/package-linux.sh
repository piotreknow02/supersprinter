#!/usr/bin/env bash
# Build the Linux release artifacts from a cargo build. Subcommands:
#
#   stage     assemble the FHS payload (binary, desktop entry, icons, license, docs)
#   tarball   supersprinter-v<V>-linux-<arch>.tar.gz of the staged tree
#   deb       hand-rolled .deb (dpkg-shlibdeps + dpkg-gencontrol + dpkg-deb)
#   rpm       binary .rpm from packaging/linux/supersprinter.spec
#   appimage  AppImage with the media (libav*/libsw*) closure bundled
#
# Every flag has a default so CI and local runs can just say
# `scripts/package-linux.sh deb --version 0.1.0`.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
TEMPLATE_DIR="${REPO_ROOT}/packaging/linux"

ICON_SIZES=(32 48 128 256 512)

# appimagetool pin (upstream AppImage/appimagetool releases).
APPIMAGETOOL_VERSION=1.9.1
APPIMAGETOOL_SHA256_x86_64=ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0
APPIMAGETOOL_SHA256_aarch64=f0837e7448a0c1e4e650a93bb3e85802546e60654ef287576f46c71c126a9158

# Media libraries the AppImage bundles next to the binary; everything else
# (libc, libstdc++, Wayland/X11, Vulkan/EGL, libxkbcommon) stays system-provided.
APPIMAGE_BUNDLE_RE='libav|libsw|libzstd|libssl|libcrypto|libmp3lame|libopus|libvorbis|libogg|libfreetype|libpng|libharfbuzz'

usage() {
  cat <<'EOF'
Usage:
  scripts/package-linux.sh <stage|tarball|deb|rpm|appimage> [options]

Options (all optional; defaults in parentheses):
  --version VERSION       Release version, 0.1.0 or 0.1.0-rc.1 (cargo metadata)
  --binary PATH           Built binary (target/release/supersprinter-gui)
  --source DIR            Source checkout with assets/ and README.md (repo root)
  --payload DIR           Staged payload directory (dist/stage)
  --out DIR               Output directory (dist/release)
  --arch ARCH             x86_64 or aarch64 (uname -m)
  --deb-revision N        .deb revision (1)
  --rpm-release N         .rpm release (1)
  --appimagetool PATH     appimagetool binary (downloaded pinned to dist/tools)

`stage` lays out the shared FHS payload: usr/bin/supersprinter, the desktop
entry (validated), hicolor icons, license and README. deb/rpm/AppImage embed
the full payload; the tarball unpacks at `/`. Pre-release versions map
X.Y.Z-rc.N to package version X.Y.Z~rc.N; file names keep `.` because GitHub
rewrites `~` on upload and checksums must match the published names.
EOF
}

fail() {
  echo "$*" >&2
  exit 1
}

require_tool() {
  command -v "$1" >/dev/null 2>&1 || fail "Required tool not found: $1"
}

require_value() {
  if [[ $# -lt 2 ]]; then
    echo "Option $1 requires a value." >&2
    exit 2
  fi
}

# One command runs per invocation, so one scratch directory is enough.
WORK=""
trap '[[ -z "$WORK" ]] || rm -rf "$WORK"' EXIT

make_workdir() {
  WORK="$(mktemp -d "${RUNNER_TEMP:-${TMPDIR:-/tmp}}/supersprinter-package.XXXXXX")"
}

validate_version() {
  [[ "$1" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-rc\.[0-9]+)?$ ]] ||
    fail "Invalid --version '$1'. Expected 0.1.0 or 0.1.0-rc.1."
}

validate_counter() {
  [[ "$2" =~ ^[1-9][0-9]*$ ]] || fail "Invalid $1 '$2'. Expected a positive integer."
}

package_version() {
  echo "${1/-rc./~rc.}"
}

asset_version() {
  echo "${1//\~/.}"
}

deb_arch_for() {
  case "$1" in
    x86_64) echo amd64 ;;
    aarch64) echo arm64 ;;
    *) fail "Unknown arch '$1' (expected x86_64 or aarch64)." ;;
  esac
}

require_payload() {
  [[ -x "$1/usr/bin/supersprinter" ]] ||
    fail "Not a staged payload (missing usr/bin/supersprinter): $1 — run 'scripts/package-linux.sh stage' first."
}

cmd_stage() {
  local out="$1" binary="$2" source="$3" size
  [[ -f "$binary" ]] || fail "Binary not found: $binary"
  [[ -d "$source/assets/linux" ]] || fail "Source checkout has no assets/linux: $source"
  if [[ -e "$out" ]] && [[ -n "$(ls -A "$out")" ]]; then
    fail "Payload directory is not empty: $out"
  fi
  require_tool desktop-file-validate

  install -Dm755 "$binary" "$out/usr/bin/supersprinter"
  install -Dm644 "$source/assets/linux/supersprinter.desktop" "$out/usr/share/applications/supersprinter.desktop"
  desktop-file-validate "$out/usr/share/applications/supersprinter.desktop"
  for size in "${ICON_SIZES[@]}"; do
    install -Dm644 "$source/assets/linux/hicolor/${size}x${size}/apps/supersprinter.png" \
      "$out/usr/share/icons/hicolor/${size}x${size}/apps/supersprinter.png"
  done
  install -Dm644 "$source/LICENSE" "$out/usr/share/licenses/supersprinter/LICENSE"
  install -Dm644 "$source/README.md" "$out/usr/share/doc/supersprinter/README.md"
  echo "Staged payload in $out"
}

cmd_tarball() {
  local payload="$1" out="$2" version="$3" arch="$4"
  local root="supersprinter-v$(asset_version "$version")-linux-${arch}" work
  make_workdir
  work="$WORK"

  # Extractable at `/`: the payload tree is the FHS layout itself.
  cp -a "$payload/." "$work/"
  tar -C "$work" -czf "$out/${root}.tar.gz" usr
  echo "Wrote $out/${root}.tar.gz"
}

write_deb_copyright() {
  local pkg_root="$1" source="$2"
  {
    echo "Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/"
    echo "Upstream-Name: supersprinter"
    echo "Upstream-Contact: piotreknow02 <piotreknow02@users.noreply.github.com>"
    echo "Source: https://github.com/piotreknow02/supersprinter"
    echo
    echo "Files: *"
    echo "Copyright: 2026 piotreknow02"
    echo "License: GPL-3.0-only"
    echo
    echo "License: GPL-3.0-only"
    sed 's/^/ /' "$source/LICENSE"
  } > "$pkg_root/usr/share/doc/supersprinter/copyright"
}

cmd_deb() {
  local payload="$1" out="$2" version="$3" revision="$4" arch="$5" source="$6"
  local deb_arch deb_version work pkg_root pkg_file
  pkg_file="$out/supersprinter-v$(asset_version "$version")-linux-${arch}.deb"
  deb_version="$(package_version "$version")-${revision}"
  deb_arch="$(deb_arch_for "$arch")"
  require_tool dpkg-shlibdeps
  require_tool dpkg-gencontrol
  require_tool dpkg-deb
  make_workdir
  work="$WORK"

  pkg_root="$work/debian/supersprinter"
  mkdir -p "$pkg_root/DEBIAN"
  cp -a "$payload/." "$pkg_root/"
  write_deb_copyright "$pkg_root" "$source"

  sed "s/@ARCH@/${deb_arch}/" "$TEMPLATE_DIR/debian-control.in" > "$work/debian/control"
  {
    echo "supersprinter (${deb_version}) unstable; urgency=medium"
    echo
    echo "  * Release ${version}."
    echo
    echo " -- piotreknow02 <piotreknow02@users.noreply.github.com>  $(date -Ru)"
  } > "$work/debian/changelog"

  (
    cd "$work"
    dpkg-shlibdeps -Tdebian/substvars debian/supersprinter/usr/bin/supersprinter
    dpkg-gencontrol -psupersprinter -Pdebian/supersprinter -Tdebian/substvars
  )
  dpkg-deb --root-owner-group --build "$pkg_root" "$pkg_file"

  local name got_version got_arch depends
  name="$(dpkg-deb -f "$pkg_file" Package)"
  got_version="$(dpkg-deb -f "$pkg_file" Version)"
  got_arch="$(dpkg-deb -f "$pkg_file" Architecture)"
  depends="$(dpkg-deb -f "$pkg_file" Depends)"
  echo "Depends: $depends"
  [[ "$name" == supersprinter ]] || fail "Unexpected .deb package name: $name"
  [[ "$got_version" == "$deb_version" ]] || fail "Unexpected .deb version: $got_version, expected $deb_version"
  [[ "$got_arch" == "$deb_arch" ]] || fail "Unexpected .deb architecture: $got_arch, expected $deb_arch"
  grep -Eq '(^|, )libav(format|codec)[0-9]' <<<"$depends" ||
    fail "Missing libav* dependency in .deb (shlibdeps did not see the media libs): $depends"
  grep -Fq 'libvulkan1 | libegl1' <<<"$depends" || fail "Missing renderer dependency in .deb: $depends"
  grep -Fq 'libxkbcommon0' <<<"$depends" || fail "Missing libxkbcommon0 dependency in .deb: $depends"
  echo "Wrote $pkg_file"
}

cmd_rpm() {
  local payload="$1" out="$2" version="$3" release="$4" arch="$5"
  local rpm_version work built pkg_file
  rpm_version="$(package_version "$version")"
  pkg_file="$out/supersprinter-v$(asset_version "$rpm_version")-linux-${arch}.rpm"
  require_tool rpmbuild
  require_tool rpm
  make_workdir
  work="$WORK"

  rpmbuild -bb \
    --define "_topdir $work" \
    --define "gc_version $rpm_version" \
    --define "gc_release $release" \
    --define "gc_payload $(cd "$payload" && pwd)" \
    --target "$arch" \
    "$TEMPLATE_DIR/supersprinter.spec"

  built="$work/RPMS/${arch}/supersprinter-${rpm_version}-${release}.${arch}.rpm"
  [[ -f "$built" ]] || fail "rpmbuild did not produce $built"
  cp "$built" "$pkg_file"

  local fields requires
  fields="$(rpm -qp --queryformat '%{NAME}|%{VERSION}|%{RELEASE}|%{ARCH}' "$pkg_file")"
  requires="$(rpm -qp --requires "$pkg_file")"
  echo "Requires:"
  echo "$requires"
  [[ "$fields" == "supersprinter|${rpm_version}|${release}|${arch}" ]] ||
    fail "Unexpected RPM metadata: $fields, expected supersprinter|${rpm_version}|${release}|${arch}"
  grep -q 'libavformat\.so' <<<"$requires" ||
    fail "Missing libav* dependency in RPM (AutoReqProv did not run)."
  grep -Fqx '(vulkan-loader or libglvnd-egl)' <<<"$requires" || fail "Missing renderer dependency in RPM."
  grep -qx 'libxkbcommon' <<<"$requires" || fail "Missing libxkbcommon dependency in RPM."
  echo "Wrote $pkg_file"
}

download_appimagetool() {
  local arch="$1" dest dir sha asset
  case "$arch" in
    x86_64) sha="$APPIMAGETOOL_SHA256_x86_64" ;;
    aarch64) sha="$APPIMAGETOOL_SHA256_aarch64" ;;
    *) fail "No appimagetool for arch '$arch'." ;;
  esac
  asset="appimagetool-${arch}.AppImage"
  dir="${REPO_ROOT}/dist/tools"
  dest="${dir}/${asset}"
  if [[ -x "$dest" ]] && echo "$sha  $dest" | sha256sum -c --status -; then
    echo "$dest"
    return
  fi
  mkdir -p "$dir"
  echo "Downloading appimagetool ${APPIMAGETOOL_VERSION} (${arch})..." >&2
  curl -fsSL "https://github.com/AppImage/appimagetool/releases/download/${APPIMAGETOOL_VERSION}/${asset}" \
    -o "$dest"
  echo "$sha  $dest" | sha256sum -c - >&2
  chmod +x "$dest"
  echo "$dest"
}

cmd_appimage() {
  local payload="$1" out="$2" version="$3" arch="$4" tool="$5"
  local appdir work lib
  [[ -x "$tool" ]] || fail "appimagetool not found or not executable: $tool"
  make_workdir
  work="$WORK"

  appdir="$work/AppDir"
  mkdir -p "$appdir"
  cp -a "$payload/." "$appdir/"
  install -m644 "$payload/usr/share/applications/supersprinter.desktop" "$appdir/supersprinter.desktop"
  install -m644 "$payload/usr/share/icons/hicolor/512x512/apps/supersprinter.png" "$appdir/supersprinter.png"

  # Bundle the media closure so the AppImage exports video without any
  # distro ffmpeg; system libs (libc/libstdc++/Wayland/X11/Vulkan) stay out.
  mkdir -p "$appdir/usr/lib"
  ldd "$appdir/usr/bin/supersprinter" |
    awk '$1 ~ /'"$APPIMAGE_BUNDLE_RE"'/ { print $3 }' |
    sort -u | while read -r lib; do
      if [[ -f "$lib" ]]; then cp -L "$lib" "$appdir/usr/lib/"; fi
    done
  cat > "$appdir/AppRun" <<'APPRUN'
#!/usr/bin/env bash
set -euo pipefail
HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
export LD_LIBRARY_PATH="${HERE}/usr/lib${LD_LIBRARY_PATH:+:${LD_LIBRARY_PATH}}"
exec "${HERE}/usr/bin/supersprinter" "$@"
APPRUN
  chmod +x "$appdir/AppRun"

  ARCH="$arch" "$tool" --appimage-extract-and-run \
    "$appdir" "$out/supersprinter-v$(asset_version "$version")-linux-${arch}.AppImage"
  echo "Wrote $out/supersprinter-v$(asset_version "$version")-linux-${arch}.AppImage"
}

[[ $# -ge 1 ]] || { usage >&2; exit 2; }
command="$1"
shift
case "$command" in
  -h|--help) usage; exit 0 ;;
  stage|tarball|deb|rpm|appimage) ;;
  *) echo "Unknown command: $command" >&2; usage >&2; exit 2 ;;
esac

version="" binary="" source="" payload="" out="" arch="" revision="1" release="1"
appimagetool=""
while [[ $# -gt 0 ]]; do
  case "$1" in
    --version) require_value "$@"; version="$2"; shift 2 ;;
    --binary) require_value "$@"; binary="$2"; shift 2 ;;
    --source) require_value "$@"; source="$2"; shift 2 ;;
    --payload) require_value "$@"; payload="$2"; shift 2 ;;
    --out) require_value "$@"; out="$2"; shift 2 ;;
    --arch) require_value "$@"; arch="$2"; shift 2 ;;
    --deb-revision) require_value "$@"; revision="$2"; shift 2 ;;
    --rpm-release) require_value "$@"; release="$2"; shift 2 ;;
    --appimagetool) require_value "$@"; appimagetool="$2"; shift 2 ;;
    -h|--help) usage; exit 0 ;;
    *) echo "Unknown arg: $1" >&2; usage >&2; exit 2 ;;
  esac
done

[[ -n "$version" ]] || version="$(cargo metadata --format-version 1 --no-deps \
  --manifest-path "${REPO_ROOT}/Cargo.toml" \
  | jq -r '.packages[] | select(.name=="supersprinter-gui").version')"
[[ -n "$binary" ]] || binary="${REPO_ROOT}/target/release/supersprinter-gui"
[[ -n "$source" ]] || source="$REPO_ROOT"
[[ -n "$payload" ]] || payload="${REPO_ROOT}/dist/stage"
[[ -n "$out" ]] || out="${REPO_ROOT}/dist/release"
[[ -n "$arch" ]] || arch="$(uname -m)"
validate_version "$version"

if [[ "$command" == stage ]]; then
  mkdir -p "$out"
  cmd_stage "$payload" "$binary" "$source"
  exit 0
fi

require_payload "$payload"
mkdir -p "$out"
case "$command" in
  tarball)
    cmd_tarball "$payload" "$out" "$version" "$arch"
    ;;
  deb)
    validate_counter --deb-revision "$revision"
    cmd_deb "$payload" "$out" "$version" "$revision" "$arch" "$source"
    ;;
  rpm)
    validate_counter --rpm-release "$release"
    cmd_rpm "$payload" "$out" "$version" "$release" "$arch"
    ;;
  appimage)
    [[ -n "$appimagetool" ]] || appimagetool="$(download_appimagetool "$arch")"
    cmd_appimage "$payload" "$out" "$version" "$arch" "$appimagetool"
    ;;
esac
