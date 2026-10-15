#!/usr/bin/env python3
"""Generates deterministic MacWin brand assets:
- assets/logo.svg      (vector brand logo)
- assets/AppIcon.icns  (macOS bundle icon with 128/256/512 PNG layers)
- assets/app.ico       (Windows icon with 16/32/48/256 PNG layers)
"""

import math
import pathlib
import struct
import zlib

ROOT = pathlib.Path(__file__).resolve().parent.parent
ASSETS = ROOT / "assets"

SVG_LOGO = """<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512" width="512" height="512">
  <defs>
    <linearGradient id="bg" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="#0f172a"/>
      <stop offset="55%" stop-color="#1e293b"/>
      <stop offset="100%" stop-color="#0b1120"/>
    </linearGradient>
    <linearGradient id="macScreen" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="#38bdf8"/>
      <stop offset="100%" stop-color="#2563eb"/>
    </linearGradient>
    <linearGradient id="winScreen" x1="0%" y1="0%" x2="100%" y2="100%">
      <stop offset="0%" stop-color="#34d399"/>
      <stop offset="100%" stop-color="#059669"/>
    </linearGradient>
    <linearGradient id="bridge" x1="0%" y1="0%" x2="100%" y2="0%">
      <stop offset="0%" stop-color="#7dd3fc"/>
      <stop offset="50%" stop-color="#f8fafc"/>
      <stop offset="100%" stop-color="#6ee7b7"/>
    </linearGradient>
  </defs>
  <!-- Squircle background -->
  <rect x="24" y="24" width="464" height="464" rx="108" fill="url(#bg)" stroke="#334155" stroke-width="8"/>
  <!-- Left monitor (macOS) -->
  <rect x="68" y="128" width="184" height="132" rx="18" fill="url(#macScreen)" stroke="#e0f2fe" stroke-width="6"/>
  <circle cx="90" cy="148" r="5" fill="#e0f2fe"/>
  <circle cx="106" cy="148" r="5" fill="#bae6fd"/>
  <path d="M136 260 L184 260 L194 284 L126 284 Z" fill="#94a3b8"/>
  <!-- Right monitor (Windows) -->
  <rect x="260" y="168" width="184" height="132" rx="14" fill="url(#winScreen)" stroke="#d1fae5" stroke-width="6"/>
  <rect x="282" y="186" width="12" height="12" rx="2" fill="#d1fae5"/>
  <rect x="298" y="186" width="12" height="12" rx="2" fill="#a7f3d0"/>
  <path d="M328 300 L376 300 L386 324 L318 324 Z" fill="#94a3b8"/>
  <!-- Seamless KVM cursor & bridge arc -->
  <path d="M168 206 C220 156, 292 156, 344 224" fill="none" stroke="url(#bridge)" stroke-width="14" stroke-linecap="round"/>
  <polygon points="242,164 294,188 266,200 278,226 260,234 248,208 230,224" fill="#ffffff" stroke="#0f172a" stroke-width="5" stroke-linejoin="round"/>
  <!-- Title badge -->
  <text x="256" y="418" text-anchor="middle" font-family="-apple-system, BlinkMacSystemFont, 'Segoe UI', sans-serif" font-size="52" font-weight="800" fill="#f8fafc" letter-spacing="2">MacWin</text>
</svg>
"""


def clamp(v: float, lo: float = 0.0, hi: float = 1.0) -> float:
    return max(lo, min(hi, v))


def sd_round_rect(px: float, py: float, cx: float, cy: float, hw: float, hh: float, r: float) -> float:
    qx = abs(px - cx) - (hw - r)
    qy = abs(py - cy) - (hh - r)
    return math.hypot(max(qx, 0.0), max(qy, 0.0)) + min(max(qx, qy), 0.0) - r


def blend(dst, src, alpha: float):
    a = clamp(alpha)
    return (
        int(dst[0] * (1.0 - a) + src[0] * a),
        int(dst[1] * (1.0 - a) + src[1] * a),
        int(dst[2] * (1.0 - a) + src[2] * a),
        int(max(dst[3], a * 255.0)),
    )


def render_rgba(size: int) -> bytes:
    buf = bytearray(size * size * 4)
    aa = 1.5 / size
    for y in range(size):
        ny = (y + 0.5) / size
        for x in range(size):
            nx = (x + 0.5) / size
            # Squircle outer shape
            d_bg = sd_round_rect(nx, ny, 0.5, 0.5, 0.45, 0.45, 0.20)
            if d_bg > aa:
                continue
            bg_alpha = clamp(0.5 - d_bg / (2 * aa))
            t = (nx + ny) * 0.5
            col = (
                int(15 * (1 - t) + 30 * t),
                int(23 * (1 - t) + 41 * t),
                int(42 * (1 - t) + 69 * t),
                int(bg_alpha * 255),
            )
            # Subtle border on squircle
            if abs(d_bg) < 0.012:
                col = blend(col, (71, 85, 105, 255), clamp(1.0 - abs(d_bg) / 0.012) * bg_alpha)

            # Left screen (Mac - sky/blue)
            d_mac = sd_round_rect(nx, ny, 0.32, 0.42, 0.17, 0.13, 0.035)
            if d_mac < aa:
                a = clamp(0.5 - d_mac / (2 * aa))
                col = blend(col, (56, 189, 248, 255), a)
            if abs(d_mac) < 0.010:
                col = blend(col, (224, 242, 254, 255), clamp(1.0 - abs(d_mac) / 0.010))

            # Right screen (Windows - emerald/teal)
            d_win = sd_round_rect(nx, ny, 0.68, 0.54, 0.17, 0.13, 0.03)
            if d_win < aa:
                a = clamp(0.5 - d_win / (2 * aa))
                col = blend(col, (16, 185, 129, 255), a)
            if abs(d_win) < 0.010:
                col = blend(col, (209, 250, 229, 255), clamp(1.0 - abs(d_win) / 0.010))

            # Center bridge badge (glowing white circle + arrow chevron)
            d_center = math.hypot(nx - 0.50, ny - 0.48) - 0.105
            if d_center < aa:
                a = clamp(0.5 - d_center / (2 * aa))
                col = blend(col, (248, 250, 252, 255), a)
                # Dark inner chevron ->
                cx = nx - 0.49
                cy = abs(ny - 0.48)
                if -0.04 <= cx <= 0.045 and abs(cx + cy - 0.025) < 0.018:
                    col = blend(col, (15, 23, 42, 255), a)
                elif abs(ny - 0.48) < 0.014 and -0.045 <= cx <= 0.02:
                    col = blend(col, (15, 23, 42, 255), a)

            i = (y * size + x) * 4
            buf[i : i + 4] = bytes((col[0], col[1], col[2], int(bg_alpha * 255)))
    return bytes(buf)


def encode_png(size: int, rgba: bytes) -> bytes:
    def chunk(tag: bytes, data: bytes) -> bytes:
        crc = zlib.crc32(tag + data) & 0xFFFFFFFF
        return struct.pack(">I", len(data)) + tag + data + struct.pack(">I", crc)

    raw = bytearray()
    stride = size * 4
    for y in range(size):
        raw.append(0)
        raw.extend(rgba[y * stride : (y + 1) * stride])
    ihdr = struct.pack(">IIBBBBB", size, size, 8, 6, 0, 0, 0)
    idat = zlib.compress(bytes(raw), 9)
    return b"\x89PNG\r\n\x1a\n" + chunk(b"IHDR", ihdr) + chunk(b"IDAT", idat) + chunk(b"IEND", b"")


def build_icns(pngs: dict[int, bytes]) -> bytes:
    # Standard macOS ICNS chunk types for PNG payloads
    tags = {16: b"icp4", 32: b"icp5", 64: b"icp6", 128: b"ic07", 256: b"ic08", 512: b"ic09"}
    body = bytearray()
    for sz, tag in tags.items():
        if sz in pngs:
            data = pngs[sz]
            body.extend(tag)
            body.extend(struct.pack(">I", len(data) + 8))
            body.extend(data)
    return b"icns" + struct.pack(">I", len(body) + 8) + bytes(body)


def build_ico(pngs: dict[int, bytes]) -> bytes:
    sizes = [16, 32, 48, 256]
    header = struct.pack("<HHH", 0, 1, len(sizes))
    entries = bytearray()
    blobs = bytearray()
    offset = 6 + 16 * len(sizes)
    for sz in sizes:
        data = pngs[sz]
        w = 0 if sz == 256 else sz
        h = 0 if sz == 256 else sz
        entries.extend(struct.pack("<BBBBHHII", w, h, 0, 0, 1, 32, len(data), offset))
        blobs.extend(data)
        offset += len(data)
    return header + bytes(entries) + bytes(blobs)


def main():
    ASSETS.mkdir(parents=True, exist_ok=True)
    (ASSETS / "logo.svg").write_text(SVG_LOGO, encoding="utf-8")
    pngs = {}
    for sz in (16, 32, 48, 64, 128, 256, 512):
        pngs[sz] = encode_png(sz, render_rgba(sz))
    (ASSETS / "AppIcon.icns").write_bytes(build_icns(pngs))
    (ASSETS / "app.ico").write_bytes(build_ico(pngs))
    print("Generated assets/logo.svg, assets/AppIcon.icns, assets/app.ico")


if __name__ == "__main__":
    main()
