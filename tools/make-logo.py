#!/usr/bin/env python3
"""Generates the VNC Beach logo and icons from a few shapes.

Writes assets/icon.svg (app icon), assets/logo.svg (icon + wordmark) and,
with --raster, PNGs and ICOs via macOS QuickLook (qlmanage) and sips.
"""
import math
import os
import shutil
import struct
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
ASSETS = os.path.join(ROOT, "assets")
DOCS = os.path.join(ROOT, "docs")

SKY_TOP, SKY_BOTTOM = "#8EDDFB", "#3FB4E6"
SEA_TOP, SEA_BOTTOM = "#1E97CC", "#0E5F8F"
SAND, SAND_SHADE = "#F4DCA7", "#E3C184"
SUN = "#FFD35C"
RED, WHITE, POLE = "#F0484B", "#FFF8EE", "#8A5A36"


def wedge(cx, cy, r, a0, a1, fill):
    """Pie slice from angle a0 to a1 (degrees, 0 = right, counter-clockwise)."""
    x0, y0 = cx + r * math.cos(math.radians(a0)), cy - r * math.sin(math.radians(a0))
    x1, y1 = cx + r * math.cos(math.radians(a1)), cy - r * math.sin(math.radians(a1))
    return (f'<path d="M{cx:.1f},{cy:.1f} L{x0:.1f},{y0:.1f} A{r},{r} 0 0 0 {x1:.1f},{y1:.1f} Z" '
            f'fill="{fill}"/>')


def umbrella(cx, cy, r, tilt):
    stripes = 6
    parts = [f'<g transform="rotate({tilt} {cx} {cy})">']
    # Pole first so the canopy covers its top.
    parts.append(f'<line x1="{cx}" y1="{cy - 30}" x2="{cx}" y2="{cy + 430}" stroke="{POLE}" '
                 f'stroke-width="26" stroke-linecap="round"/>')
    for i in range(stripes):
        a0, a1 = i * 180 / stripes, (i + 1) * 180 / stripes
        parts.append(wedge(cx, cy, r, a0, a1, RED if i % 2 == 0 else WHITE))
    # Outline and a shade band under the rim for a little depth.
    parts.append(f'<path d="M{cx - r},{cy} A{r},{r} 0 0 1 {cx + r},{cy} Z" fill="none" '
                 f'stroke="#B8323A" stroke-width="14" stroke-linejoin="round"/>')
    parts.append(f'<circle cx="{cx}" cy="{cy - r}" r="24" fill="{POLE}"/>')
    parts.append('</g>')
    return "\n".join(parts)


def icon_body():
    """The picture inside a 1024×1024 box, without the background."""
    return f"""
<defs>
  <linearGradient id="sky" x1="0" y1="0" x2="0" y2="1">
    <stop offset="0" stop-color="{SKY_TOP}"/><stop offset="1" stop-color="{SKY_BOTTOM}"/>
  </linearGradient>
  <linearGradient id="sea" x1="0" y1="0" x2="0" y2="1">
    <stop offset="0" stop-color="{SEA_TOP}"/><stop offset="1" stop-color="{SEA_BOTTOM}"/>
  </linearGradient>
  <clipPath id="box"><rect width="1024" height="1024" rx="224"/></clipPath>
</defs>
<g clip-path="url(#box)">
  <rect width="1024" height="1024" fill="url(#sky)"/>
  <circle cx="740" cy="330" r="190" fill="{SUN}" opacity="0.28"/>
  <circle cx="740" cy="330" r="125" fill="{SUN}"/>
  <path d="M-10,600 C150,560 250,640 400,600 S650,540 800,590 S980,640 1040,600 L1040,1040 L-10,1040 Z" fill="url(#sea)"/>
  <path d="M-10,600 C150,560 250,640 400,600 S650,540 800,590 S980,640 1040,600" fill="none" stroke="#FFFFFF" stroke-width="26" stroke-linecap="round" opacity="0.75"/>
  <path d="M-10,780 C200,730 380,800 560,760 S900,700 1040,760 L1040,1040 L-10,1040 Z" fill="{SAND}"/>
  <path d="M-10,780 C200,730 380,800 560,760 S900,700 1040,760" fill="none" stroke="{SAND_SHADE}" stroke-width="18" opacity="0.8"/>
  <ellipse cx="430" cy="905" rx="250" ry="42" fill="#000000" opacity="0.14"/>
  {umbrella(380, 470, 270, -14)}
</g>
"""


def icon_svg():
    return ('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1024 1024" width="1024" height="1024">'
            + icon_body() + '</svg>\n')


def logo_svg():
    # Square canvas with the logo centred: QuickLook only renders square SVGs
    # predictably. `render_png` crops the band back out.
    return f"""<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 -640 1700 1700" width="1700" height="1700" data-band="1700x420">
<g transform="translate(20 20) scale(0.371)">{icon_body()}</g>
<text x="440" y="262" font-family="-apple-system, 'Segoe UI', 'Helvetica Neue', Arial, sans-serif" font-size="196" font-weight="700" fill="#0E5F8F" letter-spacing="-4">VNC <tspan fill="{RED}">Beach</tspan></text>
<text x="446" y="350" font-family="-apple-system, 'Segoe UI', 'Helvetica Neue', Arial, sans-serif" font-size="64" font-weight="400" fill="#1E97CC">Bildschirm teilen, ganz entspannt.</text>
</svg>
"""


def render_png(svg_path, out_png, size, square=True):
    """SVG → PNG via QuickLook (longest side = size)."""
    with tempfile.TemporaryDirectory() as tmp:
        subprocess.run(["qlmanage", "-t", "-s", str(size), "-o", tmp, svg_path],
                       check=True, capture_output=True)
        produced = os.path.join(tmp, os.path.basename(svg_path) + ".png")
        shutil.copy(produced, out_png)
    if square:
        subprocess.run(["sips", "-z", str(size), str(size), out_png], check=True, capture_output=True)
    else:
        # The SVG is a square canvas with the artwork centred: crop the band.
        w, h = band_size(svg_path)
        subprocess.run(["sips", "-z", str(size), str(size), out_png], check=True, capture_output=True)
        subprocess.run(["sips", "-c", str(round(size * h / w)), str(size), out_png],
                       check=True, capture_output=True)


def band_size(path):
    import re
    m = re.search(r'data-band="(\d+)x(\d+)"', open(path).read())
    return int(m.group(1)), int(m.group(2))


def resize_png(src, out, size):
    shutil.copy(src, out)
    subprocess.run(["sips", "-z", str(size), str(size), out], check=True, capture_output=True)


def write_icns(master, out):
    """macOS icon via iconutil: every size the Finder asks for."""
    with tempfile.TemporaryDirectory() as tmp:
        iconset = os.path.join(tmp, "icon.iconset")
        os.makedirs(iconset)
        for size in (16, 32, 128, 256, 512):
            resize_png(master, os.path.join(iconset, f"icon_{size}x{size}.png"), size)
            resize_png(master, os.path.join(iconset, f"icon_{size}x{size}@2x.png"), size * 2)
        subprocess.run(["iconutil", "-c", "icns", iconset, "-o", out], check=True, capture_output=True)


def write_ico(pngs, out):
    """ICO with PNG-compressed entries (fine for Windows Vista and later)."""
    entries, data = [], []
    offset = 6 + 16 * len(pngs)
    for path, size in pngs:
        png = open(path, "rb").read()
        s = 0 if size >= 256 else size
        entries.append(struct.pack("<BBBBHHII", s, s, 0, 0, 1, 32, len(png), offset))
        data.append(png)
        offset += len(png)
    with open(out, "wb") as f:
        f.write(struct.pack("<HHH", 0, 1, len(pngs)))
        f.write(b"".join(entries))
        f.write(b"".join(data))


def main():
    os.makedirs(ASSETS, exist_ok=True)
    os.makedirs(DOCS, exist_ok=True)
    icon = os.path.join(ASSETS, "icon.svg")
    open(icon, "w").write(icon_svg())
    open(os.path.join(ASSETS, "logo.svg"), "w").write(logo_svg())
    open(os.path.join(DOCS, "favicon.svg"), "w").write(icon_svg())
    if "--raster" not in sys.argv:
        return
    master = os.path.join(ASSETS, "icon-1024.png")
    render_png(icon, master, 1024)
    sizes = [16, 24, 32, 48, 64, 128, 256]
    with tempfile.TemporaryDirectory() as tmp:
        pngs = []
        for s in sizes:
            p = os.path.join(tmp, f"{s}.png")
            resize_png(master, p, s)
            pngs.append((p, s))
        write_ico(pngs, os.path.join(ASSETS, "icon.ico"))
        write_ico([p for p in pngs if p[1] in (16, 32, 48)], os.path.join(DOCS, "favicon.ico"))
    resize_png(master, os.path.join(ASSETS, "icon-256.png"), 256)
    resize_png(master, os.path.join(ASSETS, "icon-32.png"), 32)
    write_icns(master, os.path.join(ASSETS, "icon.icns"))
    resize_png(master, os.path.join(DOCS, "apple-touch-icon.png"), 180)
    render_png(os.path.join(ASSETS, "logo.svg"), os.path.join(DOCS, "logo.png"), 1700, square=False)


if __name__ == "__main__":
    main()
