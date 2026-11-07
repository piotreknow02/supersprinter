#!/usr/bin/env bash
# Regenerate all icon assets from assets/icon.svg. Outputs are committed;
# CI and other machines never need the tools below.
#
# Canonical tools:
#   rsvg-convert  (Fedora: librsvg2-tools, Debian: librsvg2-bin)
#   icotool       (Fedora: icoutils, Debian: icoutils)
# Fallbacks when the CLIs are absent:
#   python3 + python3-gobject + librsvg (same engine as rsvg-convert)
#   python3 (built-in PNG->ICO packer)
set -euo pipefail

ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd)"
SRC="$ROOT/assets/icon.svg"
ASSETS="$ROOT/assets"
HICOLOR="$ASSETS/linux/hicolor"
HICOLOR_SIZES=(32 48 128 256 512)
ICO_SIZES=(16 32 48 256)

have() { command -v "$1" >/dev/null 2>&1; }

if ! have rsvg-convert && ! python3 -c 'import gi; gi.require_version("Rsvg", "2.0")' 2>/dev/null; then
  echo "error: need rsvg-convert (Fedora: librsvg2-tools, Debian: librsvg2-bin)" >&2
  echo "       or python3 + python3-gobject + librsvg (Fedora: python3-pygobject, Debian: python3-gi)" >&2
  exit 1
fi
if ! have icotool && ! have python3; then
  echo "error: need icotool (Fedora: icoutils, Debian: icoutils) or python3" >&2
  exit 1
fi

render() { # render <size-px> <out.png>
  local size="$1" out="$2"
  if have rsvg-convert; then
    rsvg-convert -w "$size" -h "$size" "$SRC" -o "$out"
  else
    python3 - "$SRC" "$size" "$out" <<'PY'
import sys, gi
gi.require_version("Rsvg", "2.0")
from gi.repository import Rsvg
import cairo

src, size, out = sys.argv[1], int(sys.argv[2]), sys.argv[3]
handle = Rsvg.Handle.new_from_file(src)
surf = cairo.ImageSurface(cairo.FORMAT_ARGB32, size, size)
ctx = cairo.Context(surf)
if hasattr(handle, "get_intrinsic_dimensions"):
    vb = handle.get_intrinsic_dimensions().out_viewbox
    vp = Rsvg.Rectangle()
    vp.x = 0
    vp.y = 0
    vp.width = float(size)
    vp.height = float(size)
    handle.render_document(ctx, vp)
else:
    d = handle.get_dimensions()
    ctx.scale(size / d.width, size / d.height)
    handle.render_cairo(ctx)
surf.write_to_png(out)
PY
  fi
}

tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

# Window icon used by WindowOptions.icon (gpui expects RGBA PNG).
render 256 "$ASSETS/supersprinter-window-icon.png"

# hicolor theme icons for the desktop entry.
for size in "${HICOLOR_SIZES[@]}"; do
  mkdir -p "$HICOLOR/${size}x${size}/apps"
  render "$size" "$HICOLOR/${size}x${size}/apps/supersprinter.png"
done

# Windows .ico composed from natively rendered sizes.
ico_parts=()
for size in "${ICO_SIZES[@]}"; do
  render "$size" "$tmp/${size}.png"
  ico_parts+=("$tmp/${size}.png")
done
if have icotool; then
  icotool -c -o "$ASSETS/supersprinter.ico" "${ico_parts[@]}"
else
  python3 - "$ASSETS/supersprinter.ico" "${ico_parts[@]}" <<'PY'
import struct, sys

out, *pngs = sys.argv[1:]

def png_dims(path):
    with open(path, "rb") as f:
        if f.read(8) != b"\x89PNG\r\n\x1a\n":
            raise SystemExit(f"not a PNG: {path}")
        f.read(4)  # IHDR length
        f.read(4)  # "IHDR"
        w = int.from_bytes(f.read(4), "big")
        h = int.from_bytes(f.read(4), "big")
    return w, h

blobs = [open(p, "rb").read() for p in pngs]
offset = 6 + 16 * len(blobs)
dirent = b""
for path, blob in zip(pngs, blobs):
    w, h = png_dims(path)
    dirent += struct.pack("<BBBBHHII", w % 256, h % 256, 0, 0, 1, 32, len(blob), offset)
    offset += len(blob)
with open(out, "wb") as f:
    f.write(struct.pack("<HHH", 0, 1, len(blobs)))
    f.write(dirent)
    f.write(b"".join(blobs))
PY
fi

# macOS .icns (committed so package-macos.sh needs no SVG tooling).
if have iconutil; then
  iconset="$tmp/AppIcon.iconset"
  mkdir -p "$iconset"
  render 16 "$iconset/icon_16x16.png"
  render 32 "$iconset/icon_32x32.png"
  render 64 "$iconset/icon_32x32@2x.png"
  render 128 "$iconset/icon_128x128.png"
  render 256 "$iconset/icon_256x256.png"
  render 512 "$iconset/icon_512x512.png"
  render 1024 "$iconset/icon_512x512@2x.png"
  iconutil -c icns "$iconset" -o "$ASSETS/supersprinter.icns"
else
  icns_parts=()
  for size in 16 32 64 128 256 512 1024; do
    render "$size" "$tmp/icns-${size}.png"
    icns_parts+=("$size:$tmp/icns-${size}.png")
  done
  python3 - "$ASSETS/supersprinter.icns" "${icns_parts[@]}" <<'PY'
import struct, sys

out, *parts = sys.argv[1:]
# PNG payload type codes for the square sizes we render.
types = {16: b"icp4", 32: b"icp5", 64: b"ic12", 128: b"ic07",
         256: b"ic08", 512: b"ic09", 1024: b"ic10"}
entries = b""
for part in parts:
    size, path = part.split(":", 1)
    blob = open(path, "rb").read()
    entries += types[int(size)] + struct.pack(">I", len(blob) + 8) + blob
with open(out, "wb") as f:
    f.write(b"icns" + struct.pack(">I", len(entries) + 8) + entries)
PY
fi

echo "icons regenerated under $ASSETS"
