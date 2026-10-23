#!/usr/bin/env python3
"""Draw the README's "How it works" diagram in dark and light variants.

GitHub shows README SVGs as images, which load no web fonts, so every label is
shaped with HarfBuzz from the site's Geist fonts (docs/assets/fonts) and
written as outlines. Edit the copy or layout here and run the script again;
don't edit the SVGs by hand.

Needs fonttools, brotli and uharfbuzz (pip install fonttools brotli uharfbuzz).
Writes docs/assets/readme/how-it-works-{dark,light}.svg.
"""
import io
import pathlib

import uharfbuzz as hb
from fontTools.pens.svgPathPen import SVGPathPen
from fontTools.pens.transformPen import TransformPen
from fontTools.ttLib import TTFont

ROOT = pathlib.Path(__file__).resolve().parents[1]
FONTS = ROOT / "docs/assets/fonts"
OUT = ROOT / "docs/assets/readme"

W, H = 1280, 580

THEMES = {
    "dark": {
        "bg": "#0A0A0B",
        "card_border": "rgba(255,255,255,0.08)",
        "dots": "rgba(255,255,255,0.075)",
        "glow": "#E8FF47",
        "glow_opacity": 0.09,
        "frame": "#121214",
        "frame_stroke": "rgba(255,255,255,0.14)",
        "screen": "#0A0A0B",
        "screen_stroke": "rgba(255,255,255,0.06)",
        "hairline": "rgba(255,255,255,0.08)",
        "text": "#F4F4F5",
        "muted": "#A1A1AA",
        "faint": "#8F8F98",
        "out_chip": "rgba(232,255,71,0.10)",
        "out_chip_stroke": "rgba(232,255,71,0.38)",
        "out_num": "#E8FF47",
        "out_link": "rgba(232,255,71,0.35)",
        "ret_chip": "#1A1A1D",
        "ret_chip_stroke": "rgba(255,255,255,0.14)",
        "ret_num": "#A1A1AA",
        "video": "#E8FF47",
        "input": "#A1A1AA",
        "pill": "#E8FF47",
        "pill_stroke": "none",
        "pill_text": "#0C0D04",
        "chip": "#121214",
        "chip_stroke": "rgba(255,255,255,0.16)",
        "chip_text": "#F4F4F5",
        "port": "#0A0A0B",
    },
    "light": {
        "bg": "#FFFFFF",
        "card_border": "rgba(0,0,0,0.09)",
        "dots": "rgba(0,0,0,0.08)",
        "glow": "#E8FF47",
        "glow_opacity": 0.14,
        "frame": "#F4F4F5",
        "frame_stroke": "rgba(0,0,0,0.13)",
        "screen": "#FFFFFF",
        "screen_stroke": "rgba(0,0,0,0.07)",
        "hairline": "rgba(0,0,0,0.08)",
        "text": "#09090B",
        "muted": "#52525B",
        "faint": "#63636B",
        "out_chip": "#E8FF47",
        "out_chip_stroke": "rgba(0,0,0,0.10)",
        "out_num": "#0C0D04",
        "out_link": "rgba(92,107,0,0.40)",
        "ret_chip": "#F4F4F5",
        "ret_chip_stroke": "rgba(0,0,0,0.13)",
        "ret_num": "#52525B",
        "video": "#5C6B00",
        "input": "#71717A",
        "pill": "#E8FF47",
        "pill_stroke": "rgba(0,0,0,0.12)",
        "pill_text": "#0C0D04",
        "chip": "#FFFFFF",
        "chip_stroke": "rgba(0,0,0,0.14)",
        "chip_text": "#09090B",
        "port": "#FFFFFF",
    },
}


class Font:
    def __init__(self, filename):
        self.tt = TTFont(FONTS / filename)
        self.tt.flavor = None
        data = io.BytesIO()
        self.tt.save(data)
        self.hb = hb.Font(hb.Face(data.getvalue()))
        self.upem = self.tt["head"].unitsPerEm
        self.glyphs = self.tt.getGlyphSet()

    def shape(self, text):
        buf = hb.Buffer()
        buf.add_str(text)
        buf.guess_segment_properties()
        hb.shape(self.hb, buf, {"kern": True, "liga": True})
        return list(zip(buf.glyph_infos, buf.glyph_positions))

    def width(self, text, size):
        return sum(p.x_advance for _, p in self.shape(text)) * size / self.upem

    def outline(self, text, size, x, y):
        scale = size / self.upem
        pen = SVGPathPen(self.glyphs, ntos=lambda v: f"{v:.1f}".rstrip("0").rstrip("."))
        for info, pos in self.shape(text):
            name = self.tt.getGlyphName(info.codepoint)
            gx = x + pos.x_offset * scale
            gy = y - pos.y_offset * scale
            self.glyphs[name].draw(TransformPen(pen, (scale, 0, 0, -scale, gx, gy)))
            x += pos.x_advance * scale
        return pen.getCommands()


REGULAR = Font("Geist-Regular.woff2")
MEDIUM = Font("Geist-Medium.woff2")
SEMIBOLD = Font("Geist-SemiBold.woff2")
MONO = Font("GeistMono-Regular.woff2")
MONO_MEDIUM = Font("GeistMono-Medium.woff2")


def text(font, size, value, x, y, fill, anchor="start", max_width=None):
    width = font.width(value, size)
    if max_width is not None and width > max_width:
        raise SystemExit(f"{value!r} is {width:.0f}px wide, over its {max_width}px")
    if anchor == "middle":
        x -= width / 2
    elif anchor == "end":
        x -= width
    return f'<path fill="{fill}" d="{font.outline(value, size, x, y)}"/>'


# Each step: number, title, description, centre y. The PC and iPad columns
# share row positions so the return path runs straight from step 07 to 08.
PC_STEPS = [
    ("01", "Capture", "Duplicates the desktop with DXGI", 152, "out"),
    ("02", "Encode", "On the graphics card, through FFmpeg", 208, "out"),
    ("03", "Send", "Splits each frame into numbered fragments", 264, "out"),
    ("08", "Inject input", "Replays touch, keys and Pencil on Windows", 348, "ret"),
]
IPAD_STEPS = [
    ("04", "Reassemble and repair", "Rebuilds frames, requests lost fragments", 152, "out"),
    ("05", "Decode", "In hardware with VideoToolbox", 208, "out"),
    ("06", "Display", "Draws every frame with Metal", 264, "out"),
    ("07", "Input", "Touch, keyboard and Apple Pencil", 348, "ret"),
]

PC = {"x": 72, "y": 96, "w": 448, "h": 316, "screen": (84, 108, 424, 276)}
IPAD = {"x": 760, "y": 100, "w": 448, "h": 316, "screen": (776, 116, 416, 284)}
DIVIDER_Y = 300
VIDEO_FROM = (PC["x"] + PC["w"], 264)
VIDEO_TO = (IPAD["x"], 152)
INPUT_Y = 348


def steps(t, steps_, screen):
    sx, _, sw, _ = screen
    chip_x, title_x = sx + 24, sx + 64
    max_width = sx + sw - 20 - title_x
    out = []
    outbound = [s for s in steps_ if s[4] == "out"]
    for a, b in zip(outbound, outbound[1:]):
        out.append(
            f'<line x1="{chip_x + 14}" y1="{a[3] + 18}" x2="{chip_x + 14}" '
            f'y2="{b[3] - 18}" stroke="{t["out_link"]}" stroke-width="1.5" '
            'stroke-linecap="round" stroke-dasharray="2 4"/>'
        )
    for number, title, desc, cy, kind in steps_:
        fill, stroke, num = (
            (t["out_chip"], t["out_chip_stroke"], t["out_num"])
            if kind == "out"
            else (t["ret_chip"], t["ret_chip_stroke"], t["ret_num"])
        )
        out.append(
            f'<rect x="{chip_x}" y="{cy - 14}" width="28" height="28" rx="8" '
            f'fill="{fill}" stroke="{stroke}"/>'
        )
        out.append(text(MONO_MEDIUM, 12, number, chip_x + 14, cy + 4, num, "middle"))
        out.append(text(MEDIUM, 17, title, title_x, cy - 4, t["text"], max_width=max_width))
        out.append(text(REGULAR, 14.5, desc, title_x, cy + 17, t["muted"], max_width=max_width))
    return out


def monitor(t):
    x, y, w, h = PC["x"], PC["y"], PC["w"], PC["h"]
    sx, sy, sw, sh = PC["screen"]
    cx = x + w / 2
    neck_top, neck_bottom = y + h, y + h + 34
    return [
        f'<path d="M{cx - 26} {neck_top} L{cx + 26} {neck_top} L{cx + 34} {neck_bottom} '
        f'L{cx - 34} {neck_bottom} Z" fill="{t["frame"]}" stroke="{t["frame_stroke"]}"/>',
        f'<rect x="{cx - 76}" y="{neck_bottom}" width="152" height="9" rx="4.5" '
        f'fill="{t["frame"]}" stroke="{t["frame_stroke"]}"/>',
        f'<rect x="{x + 0.5}" y="{y + 0.5}" width="{w - 1}" height="{h - 1}" rx="16" '
        f'fill="{t["frame"]}" stroke="{t["frame_stroke"]}"/>',
        f'<rect x="{sx}" y="{sy}" width="{sw}" height="{sh}" rx="6" '
        f'fill="{t["screen"]}" stroke="{t["screen_stroke"]}"/>',
        f'<circle cx="{cx}" cy="{sy + sh + (y + h - sy - sh) / 2}" r="2.5" fill="{t["frame_stroke"]}"/>',
    ]


def ipad(t):
    x, y, w, h = IPAD["x"], IPAD["y"], IPAD["w"], IPAD["h"]
    sx, sy, sw, sh = IPAD["screen"]
    return [
        f'<rect x="{x + 0.5}" y="{y + 0.5}" width="{w - 1}" height="{h - 1}" rx="30" '
        f'fill="{t["frame"]}" stroke="{t["frame_stroke"]}"/>',
        f'<rect x="{sx}" y="{sy}" width="{sw}" height="{sh}" rx="16" '
        f'fill="{t["screen"]}" stroke="{t["screen_stroke"]}"/>',
        f'<circle cx="{x + w / 2}" cy="{y + 8}" r="2.5" fill="{t["frame_stroke"]}"/>',
    ]


def divider(t, screen):
    sx, _, sw, _ = screen
    return f'<line x1="{sx + 24}" y1="{DIVIDER_Y}" x2="{sx + sw - 24}" y2="{DIVIDER_Y}" stroke="{t["hairline"]}"/>'


def port(t, x, y, color):
    return f'<circle cx="{x}" cy="{y}" r="4" fill="{t["port"]}" stroke="{color}" stroke-width="1.5"/>'


def pill(t, label, cx, cy, font, fill, stroke, color):
    w = font.width(label, 14.5) + 32
    return [
        f'<rect x="{cx - w / 2:.1f}" y="{cy - 16}" width="{w:.1f}" height="32" rx="16" '
        f'fill="{fill}" stroke="{stroke}"/>',
        text(font, 14.5, label, cx, cy + 5, color, "middle"),
    ]


def links(t):
    (x1, y1), (x2, y2) = VIDEO_FROM, VIDEO_TO
    mid = (x1 + x2) / 2
    video = f"M{x1} {y1} C{mid} {y1} {mid} {y2} {x2 - 6} {y2}"
    out = [
        f'<path d="{video}" fill="none" stroke="{t["video"]}" stroke-width="2" '
        'stroke-linecap="round" stroke-dasharray="10 7"/>',
        f'<path d="M{x2 - 13} {y2 - 6} L{x2 - 6} {y2} L{x2 - 13} {y2 + 6}" fill="none" '
        f'stroke="{t["video"]}" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>',
        port(t, x1, y1, t["video"]),
        f'<line x1="{x2}" y1="{INPUT_Y}" x2="{x1 + 6}" y2="{INPUT_Y}" stroke="{t["input"]}" '
        'stroke-width="1.5" stroke-linecap="round"/>',
        f'<path d="M{x1 + 13} {INPUT_Y - 6} L{x1 + 6} {INPUT_Y} L{x1 + 13} {INPUT_Y + 6}" fill="none" '
        f'stroke="{t["input"]}" stroke-width="1.5" stroke-linecap="round" stroke-linejoin="round"/>',
        port(t, x2, INPUT_Y, t["input"]),
    ]
    out += pill(t, "Video and audio", mid, (y1 + y2) / 2, MEDIUM, t["pill"], t["pill_stroke"], t["pill_text"])
    out.append(text(MEDIUM, 14.5, "Input and repair requests", mid, INPUT_Y + 27, t["chip_text"], "middle"))
    out += transport(t, mid, 290)
    return out


def transport(t, cx, cy):
    label = "Wi-Fi or USB"
    lw = MEDIUM.width(label, 14.5)
    w = lw + 28 + 46
    left = cx - w / 2
    wx, ux = left + 16, left + 36  # Wi-Fi and USB-C icon centres
    stroke = f'fill="none" stroke="{t["muted"]}" stroke-width="1.4" stroke-linecap="round"'
    return [
        f'<rect x="{left:.1f}" y="{cy - 17}" width="{w:.1f}" height="34" rx="10" '
        f'fill="{t["chip"]}" stroke="{t["chip_stroke"]}" stroke-dasharray="3 3"/>',
        f'<path d="M{wx - 7} {cy - 1.5} A10 10 0 0 1 {wx + 7} {cy - 1.5}" {stroke}/>',
        f'<path d="M{wx - 4} {cy + 1.5} A5.5 5.5 0 0 1 {wx + 4} {cy + 1.5}" {stroke}/>',
        f'<circle cx="{wx}" cy="{cy + 4.5}" r="1.4" fill="{t["muted"]}"/>',
        f'<rect x="{ux - 7}" y="{cy - 3.5}" width="14" height="7" rx="3.5" {stroke}/>',
        f'<line x1="{ux - 3}" y1="{cy}" x2="{ux + 3}" y2="{cy}" {stroke}/>',
        text(MEDIUM, 14.5, label, left + 54, cy + 5, t["chip_text"]),
    ]


def headers(t):
    out = []
    for x, name, detail in ((PC["x"], "Windows PC", "Rust host"), (IPAD["x"], "iPad", "Swift app")):
        out.append(text(SEMIBOLD, 18, name, x + 2, 66, t["text"]))
        out.append(text(MONO, 13.5, detail, x + 2 + SEMIBOLD.width(name, 18) + 12, 66, t["faint"]))
    return out


def footer(t):
    y = 532
    out = [f'<line x1="40" y1="488" x2="{W - 40}" y2="488" stroke="{t["hairline"]}"/>']
    out.append(
        f'<line x1="74" y1="{y - 5}" x2="98" y2="{y - 5}" stroke="{t["video"]}" '
        'stroke-width="2" stroke-linecap="round" stroke-dasharray="7 5"/>'
    )
    out.append(text(REGULAR, 14.5, "PC to iPad", 108, y, t["muted"]))
    x = 108 + REGULAR.width("PC to iPad", 14.5) + 32
    out.append(
        f'<line x1="{x}" y1="{y - 5}" x2="{x + 24}" y2="{y - 5}" stroke="{t["input"]}" '
        'stroke-width="1.5" stroke-linecap="round"/>'
    )
    out.append(text(REGULAR, 14.5, "iPad to PC", x + 34, y, t["muted"]))
    out.append(text(MONO, 13.5, "UDP fragments on Wi-Fi · framed tunnel on USB", W - 72, y, t["faint"], "end"))
    return out


def draw(name, t):
    alt = (
        "The Windows PC captures the desktop and encodes it on the GPU, then sends "
        "video and audio over Wi-Fi or USB to the iPad, which reassembles and repairs "
        "the stream, decodes it with VideoToolbox and shows it with Metal. Touch, "
        "keyboard and Pencil input and repair requests travel back to the PC, which "
        "injects the input."
    )
    body = [
        f'<rect x="0.5" y="0.5" width="{W - 1}" height="{H - 1}" rx="24" fill="{t["bg"]}" '
        f'stroke="{t["card_border"]}"/>',
        f'<rect width="{W}" height="{H}" rx="24" fill="url(#dots)" mask="url(#fade)"/>',
        f'<ellipse cx="640" cy="230" rx="300" ry="190" fill="url(#glow)"/>',
        *headers(t),
        *monitor(t),
        *ipad(t),
        divider(t, PC["screen"]),
        divider(t, IPAD["screen"]),
        *steps(t, PC_STEPS, PC["screen"]),
        *steps(t, IPAD_STEPS, IPAD["screen"]),
        *links(t),
        *footer(t),
    ]
    svg = (
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" '
        f'viewBox="0 0 {W} {H}" role="img" aria-labelledby="title desc">\n'
        f'<title id="title">How EternalMonitor works</title>\n'
        f'<desc id="desc">{alt}</desc>\n'
        "<defs>\n"
        '<pattern id="dots" width="24" height="24" patternUnits="userSpaceOnUse">'
        f'<circle cx="12" cy="12" r="1" fill="{t["dots"]}"/></pattern>\n'
        '<radialGradient id="fadeGrad" cx="50%" cy="42%" r="62%">'
        '<stop offset="0" stop-color="#fff"/><stop offset="1" stop-color="#fff" stop-opacity="0"/>'
        "</radialGradient>\n"
        f'<mask id="fade"><rect width="{W}" height="{H}" fill="url(#fadeGrad)"/></mask>\n'
        '<radialGradient id="glow">'
        f'<stop offset="0" stop-color="{t["glow"]}" stop-opacity="{t["glow_opacity"]}"/>'
        f'<stop offset="1" stop-color="{t["glow"]}" stop-opacity="0"/></radialGradient>\n'
        "</defs>\n" + "\n".join(body) + "\n</svg>\n"
    )
    path = OUT / f"how-it-works-{name}.svg"
    path.write_text(svg)
    print(f"{path.relative_to(ROOT)}: {len(svg.encode()) / 1024:.0f} KB")


if __name__ == "__main__":
    OUT.mkdir(parents=True, exist_ok=True)
    for theme, tokens in THEMES.items():
        draw(theme, tokens)
