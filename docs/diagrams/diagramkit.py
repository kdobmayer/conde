"""diagramkit: draw an architecture or flow diagram as one self-contained SVG from a Python script.

A build script does `from diagramkit import *`, calls `header`, the box and node helpers and `edge`, then
`write(name, W, H)`. Every helper appends SVG to the list P (defs go to D); colours come from the dict C,
set by `set_palette`. Coordinates are canvas px; text sizes are px at 1x.

Primitives
  rect(x, y, w, h, fill, stroke, sw, r, dash, opacity)   rounded rectangle
  line(x1, y1, x2, y2, color, sw, dash)                  straight line
  text(x, y, value, size, weight, color, anchor, spacing, italic)   one line of Inter
  mono(x, y, value, size, color, weight, anchor)          one line of IBM Plex Mono, spaces kept
  image(x, y, name, size, opacity)                        icon by name, see Icons below
  hexagon(cx, cy, r, fill, stroke, sw)                    flat hexagon
  tw(value, size, weight, mono)                           advance width of a label in canvas px
  esc(s)                                                  XML escape
Edges
  edge(points, color, label, dash, width, label_at, ...)  orthogonal polyline with arrow and boxed label
  label(x, y, rows, color, size, anchor, box)             edge label placed away from its edge
  marker(color)                                           arrowhead def for a colour (edge calls it)
Containers
  header(W, title, subtitle, right1)                      white page, title block and the header rule
  section(x, y, w, h, title, kicker, fill, stroke, dash, kcolor)   large group with kicker and title
  card(x, y, w, h, title, fill, stroke, dash, note, tcolor)        group with caps title and a rule
  boundary(x, y, w, h, title, note, fill, color, icon, dash, sw)   network or account boundary
  cluster_box(x, y, w, h, title, note, icon)              cluster frame, icon optional
  account_box(x, y, w, h, title, icon)                    account frame, icon optional
  zone_band(x, y, w, h, name, note)                       dashed availability zone band
  legend(x, y, w, h, rows, kicker)                        legend section; rows are (label, kind)
Nodes
  node(cx, y, icon, label, note, size, muted, lsize, nsize)   icon over centred label and note
  mini(cx, y, icon, label, note)                          small node
  hnode(x, y, icon, label, note, size, lsize, nsize)      icon left, text right
  pod(cx, y, label, note, color, stack, muted, ...)       workload hexagon, stack draws replicas
  glyph_pod(cx, y, label, note, glyph, stack, ghost, ...) hexagon with a white glyph: box clock mail list link shield swap
  hpod(x, y, glyph, label, note, size, stack, ...)        glyph_pod left, text right; returns the centre
  lines_at(x, y, label, note, lsize, nsize, anchor)       bold rows then muted rows; returns the next y
  chip(x, y, label, fill, color, anchor, size)            pill with white text; returns its width
  badge(cx, cy, n, fill, r)                               numbered circle
  table(x, y, w, name, cols, note, rows, color)           database table glyph; returns its height
Output
  set_palette(name)                                       reset C to the base and apply a palette
  write(name, W, H, out=None)                             write <out>/<name>.svg after the checks

Icons, by name
  Arch_Amazon-RDS_64, Res_Users_48_Light, AWS-Account_32   a bare file stem, searched recursively in ICON_DIRS
  si:<slug>     Simple Icons mark, filled with its brand colour (BRAND, extended from the package data)
  az:<name>     Azure Public Service Icons: the stem, or the part after "-icon-service-" (az:Virtual-Machine)
  gcp:<name>    Google Cloud icons: the stem, or the stem without "-512-color" (gcp:CloudSQL, gcp:compute_engine)
  glyph:<name>  built-in neutral glyph: reload server users browser queue db file lock clock mail cloud
ICON_DIRS: $DIAGRAM_ICONS (colon separated), then ~/.cache/diagram-icons/ (kit/icons.sh fills it),
then kit/icons/ next to this module (the offline fallback set, listed in kit/icons/INDEX.md).
"""
import base64
import html
import json
import math
import os
import pathlib
import re
import subprocess
import sys

KIT_DIR = pathlib.Path(__file__).resolve().parent
ICON_DIRS = [pathlib.Path(p).expanduser() for p in os.environ.get("DIAGRAM_ICONS", "").split(":") if p]
ICON_DIRS += [pathlib.Path("~/.cache/diagram-icons").expanduser(), KIT_DIR / "icons"]
MONO = "IBM Plex Mono,Liberation Mono,monospace"
SANS = "Inter,Segoe UI,Arial,sans-serif"
GLYPH = "#6F7E91"                                   # stroke colour of the built-in glyphs

P = []
D = []
C = {
    "ink": "#122033", "muted": "#5E6B7C", "quiet": "#8290A3",
    "line": "#DCE4ED", "blue": "#1769C2", "blue2": "#3F7FC4",
    "blue_fill": "#F7F9FC", "navy": "#153B68", "purple": "#7650B5",
    "green": "#13866F", "orange": "#E56B18", "grey": "#8491A3",
    "pending": "#F5F6F8", "white": "#FFFFFF", "k8s": "#326CE5",
    "zone": "#6D93BE", "zone_fill": "#F5F8FC",
}
# Brand colours that win over the Simple Icons package data.
BRAND = {"kubernetes": "#326CE5", "gitlab": "#FC6D26", "argo": "#EF7B4D",
         "fluentbit": "#49BDA5", "helm": "#0F1689", "grafana": "#F46800", "prometheus": "#E6522C"}
# Colour roles. Flows: app (main traffic), carrier (a second traffic class), data, deploy.
C.update({
    "app": "#1769C2", "carrier": "#E56B18", "data": "#13866F", "deploy": "#8491A3",
    "account": "#3F7FC4", "vpc": "#3F7FC4", "vpc_fill": "#F7F9FC", "zone_text": "#153B68",
    "cluster": "#1769C2", "cluster_fill": "#F8FAFE", "web_fill": "#F1F6FE", "web_stroke": "#9EBEE3",
    "pod": "#326CE5", "card_title": "#5E6B7C", "legend": "#8491A3",
    "sec_ext": "#3F7FC4", "kick_ext": "#3F7FC4", "sec_carrier": "#E56B18", "kick_carrier": "#E56B18",
    "sec_cloud": "#153B68", "kick_cloud": "#153B68", "sec_data": "#13866F", "kick_data": "#13866F",
    "data_fill": "#F4FBF9", "warm_card": "#E7B48F", "warm_title": "#5E6B7C", "warm_text": "#E56B18",
    # step bands and chevrons
    "band_fill": "#F7F9FC", "band_stroke": "#9BB9DC", "band_text": "#153B68", "human_stroke": "#E3C4A5",
    "human_text": "#8A4B12", "chev": ["#D2DFEE", "#8FB0D8", "#F3E3D3", "#153B68"],
    "chev_text": ["#153B68", "#FFFFFF", "#8A4B12", "#FFFFFF"], "rollback_fill": "#FFF7F0", "rollback": "#E56B18",
    "rollback_text": "#8A4B12", "tile_fill": "#F7F9FC",
    # order strip
    "step_fill": "#EEF1F4", "step_on_fill": "#E3F0EE", "step_on_text": "#115E59", "step_on_num": "#0F766E",
})
_BASE = dict(C)
_NEUTRAL = {"ink": "#182230", "muted": "#667085", "quiet": "#98A2B3", "line": "#E4E7EC", "card_title": "#667085",
            "legend": "#98A2B3", "zone_text": "#344054"}
_WARM = {"ink": "#182230", "muted": "#6F6861", "quiet": "#9A928A", "line": "#E7E1DA", "card_title": "#6F6861",
         "legend": "#A39A91", "zone_text": "#3F3A36", "data": "#A39A91", "deploy": "#A39A91",
         "account": "#CFC7BD", "vpc": "#D6CEC4", "vpc_fill": "#F7F5F2", "zone": "#DDD6CE", "zone_fill": "#F7F5F2",
         "cluster_fill": "#F3F0EC", "web_fill": "#F3F0EC", "web_stroke": "#DDD6CE",
         "sec_ext": "#DDD6CE", "sec_carrier": "#DDD6CE", "sec_cloud": "#DDD6CE", "sec_data": "#DDD6CE",
         "data_fill": "#F7F5F2", "warm_card": "#DDD6CE", "warm_title": "#6F6861",
         "band_fill": "#F7F5F2", "band_stroke": "#DDD6CE", "human_stroke": "#DDD6CE",
         "rollback_fill": "#F7F5F2", "tile_fill": "#F7F5F2", "step_fill": "#F3F0EC"}


def _graphite(accent, accent_dark, accent_tint, carrier, pod, **extra):
    d = dict(_WARM, **{
        "app": accent, "carrier": carrier, "cluster": accent, "pod": pod, "warm_text": carrier,
        "kick_ext": accent, "kick_carrier": accent, "kick_cloud": accent, "kick_data": accent,
        "band_text": accent, "human_text": accent, "chev": ["#E9E3DC", "#E0D9D1", "#D6CEC4", accent],
        "chev_text": ["#182230", "#182230", "#182230", "#FFFFFF"], "rollback": accent, "rollback_text": accent_dark,
        "step_on_fill": accent_tint, "step_on_text": accent_dark, "step_on_num": accent})
    d.update(extra)
    return d


PALETTES = {
    # kit: the base colours; blue app traffic, orange second traffic class, green data, grey deploy
    "kit": {},
    # kit-blue (default): kit with its orange replaced by kit blues; a calm one-hue page for slides and sites
    "kit-blue": {
        "carrier": "#153B68", "sec_carrier": "#153B68", "kick_carrier": "#153B68",
        "warm_card": "#9EBEE3", "warm_text": "#153B68",
        "human_stroke": "#9BB9DC", "human_text": "#153B68",
        "chev": ["#D2DFEE", "#8FB0D8", "#355E8D", "#153B68"], "chev_text": ["#153B68", "#FFFFFF", "#FFFFFF", "#FFFFFF"],
        "rollback": "#1769C2", "rollback_fill": "#F7F9FC", "rollback_text": "#153B68"},
    # p3: cool neutrals, one teal accent, three edge colours (teal, slate, grey)
    "p3": dict(_NEUTRAL, **{
        "app": "#0F766E", "carrier": "#475467", "data": "#8A94A6", "deploy": "#8A94A6",
        "account": "#98A2B3", "vpc": "#C1C8D2", "vpc_fill": "#FAFBFC", "zone": "#CDD3DC", "zone_fill": "#F9FAFB",
        "cluster": "#0F766E", "cluster_fill": "#F5F9F8", "web_fill": "#F5F9F8", "web_stroke": "#CFE3E0",
        "pod": "#5B6B82", "sec_ext": "#D0D5DD", "kick_ext": "#667085", "sec_carrier": "#D0D5DD",
        "kick_carrier": "#667085", "sec_cloud": "#D0D5DD", "kick_cloud": "#667085", "sec_data": "#D0D5DD",
        "kick_data": "#667085", "data_fill": "#F9FAFB", "warm_card": "#D0D5DD", "warm_title": "#667085",
        "warm_text": "#475467"}),
    # p3-g1: warm graphite neutrals with a terracotta accent; for print documents
    "p3-g1": _graphite("#9E5A43", "#7F4533", "#F2E7E2", "#475467", "#4A5260"),
    # p3-g2: warm graphite neutrals, the accent is the ink itself; the quietest option
    "p3-g2": _graphite("#182230", "#182230", "#E9E3DC", "#8C8279", "#4A5260", data="#B8AFA5", deploy="#B8AFA5",
                       legend="#B8AFA5"),
}
LANDING_PALETTE = "kit-blue"                         # web pages and slides
DOC_PALETTE = "p3-g1"                                # printed documents
FLOW = {}


def set_palette(name):
    C.clear()
    C.update(_BASE)
    C.update(PALETTES[name])
    FLOW.update(app=C["app"], carrier=C["carrier"], data=C["data"], deploy=C["deploy"])


set_palette(LANDING_PALETTE)


def esc(s): return html.escape(str(s), quote=True)


# Fonts: label widths are measured with the font rsvg-convert draws with, found through fontconfig.
_fonts = {}
_warned = []
_CSS_TO_FC = [(350, "light"), (450, "regular"), (550, "medium"), (650, "semibold"), (750, "bold"),
              (850, "extrabold"), (10 ** 4, "black")]


def _fc_match(pattern):
    try:
        out = subprocess.run(["fc-match", "-f", "%{file}|%{index}|%{family}", pattern],
                             capture_output=True, text=True, check=True).stdout
    except (OSError, subprocess.CalledProcessError):
        return None
    path, index, family = (out.split("|", 2) + ["", ""])[:3]
    return path, int(index or 0), family


def _load_font(mono, weight):
    from PIL import ImageFont
    if mono:
        hit = _fc_match("IBM Plex Mono")
        if hit and "IBM Plex Mono" in hit[2]:
            return ImageFont.truetype(hit[0], 100, index=hit[1])
    else:
        hit = _fc_match("Inter Variable")
        if hit and hit[2].startswith("Inter"):
            f = ImageFont.truetype(hit[0], 100, index=hit[1])
            try:
                axes = f.get_variation_axes()
            except OSError:
                axes = None
            if axes:
                f.set_variation_by_axes([_axis_value(a, weight) for a in axes])
                return f
        style = next(n for limit, n in _CSS_TO_FC if weight < limit)
        hit = _fc_match(f"Inter:{style}")
        if hit and hit[2].startswith("Inter"):
            return ImageFont.truetype(hit[0], 100, index=hit[1])
    if not _warned:
        print("diagramkit: Inter or IBM Plex Mono not found, measuring with DejaVu Sans", file=sys.stderr)
        _warned.append(1)
    hit = _fc_match("DejaVu Sans")
    return ImageFont.truetype(hit[0], 100, index=hit[1]) if hit else ImageFont.load_default(100)


def _axis_value(axis, weight):
    name = axis.get("name", b"")
    name = name.decode() if isinstance(name, bytes) else str(name)
    lo, hi = axis["minimum"], axis["maximum"]
    if name.lower().startswith("weight"):
        return max(lo, min(hi, weight))
    if name.lower().startswith("optical"):
        return max(lo, min(hi, 14))                  # text size optics, as rsvg renders labels
    return axis["default"]


def tw(value, size, weight=400, mono=False):
    """Advance width of a label in canvas px (Inter or IBM Plex Mono, as rsvg renders them)."""
    key = (mono, weight)
    if key not in _fonts:
        _fonts[key] = _load_font(mono, weight)
    return _fonts[key].getlength(str(value)) * size / 100


# Icons
_icons = {}
_index = {}
_brand_loaded = []


def _stems(d):
    """Map file stem to path for every SVG under d (first in sorted order wins), built once per dir."""
    if d not in _index:
        found = {}
        if d.is_dir():
            for p in sorted(d.rglob("*.svg")):
                if "__MACOSX" not in p.parts:
                    found.setdefault(p.stem, p)
        _index[d] = found
    return _index[d]


def _find(keys, subdir=None, suffix=None):
    """First icon whose stem is one of keys, or ends with suffix, in ICON_DIRS order."""
    for d in ICON_DIRS:
        root = d / subdir if subdir and (d / subdir).is_dir() else d
        stems = _stems(root)
        for key in keys:
            if key in stems:
                return stems[key]
        if suffix:
            hit = next((p for s, p in stems.items() if s.endswith(suffix)), None)
            if hit:
                return hit
    return None


def _load_brand():
    if _brand_loaded:
        return
    _brand_loaded.append(1)
    for d in ICON_DIRS:
        for rel in ("si/package/data/simple-icons.json", "si/package/_data/simple-icons.json"):
            f = d / rel
            if f.exists():
                data = json.loads(f.read_text())
                for item in data.get("icons", []) if isinstance(data, dict) else data:
                    if item.get("slug") and item.get("hex"):
                        BRAND.setdefault(item["slug"], "#" + item["hex"])
                return


def _si_path(slug):
    for d in ICON_DIRS:
        for p in (d / "si/package/icons" / f"{slug}.svg", d / "si" / f"{slug}.svg", d / f"si__{slug}.svg"):
            if p.exists():
                return p
    return None


def icon_uri(name):
    """Data URI of an icon by name (see the module docstring for the name forms)."""
    if name in _icons:
        return _icons[name]
    if name.startswith("si:"):
        slug = name[3:]
        path = _si_path(slug)
        if path:
            svg = path.read_text()
            if not re.match(r"<svg[^>]*\sfill=", svg):
                _load_brand()
                svg = svg.replace("<svg ", f'<svg fill="{BRAND.get(slug, "#334155")}" ', 1)
            data = svg.encode()
    else:
        if name.startswith("az:"):
            path = _find([name[3:]], "azure", "-icon-service-" + name[3:])
        elif name.startswith("gcp:"):
            path = _find([name[4:], name[4:] + "-512-color"], "gcp")
        else:
            path = _find([name])
        data = path.read_bytes() if path else None
    if not path:
        raise SystemExit(f"icon not found: {name}; searched " + ", ".join(str(d) for d in ICON_DIRS))
    _icons[name] = "data:image/svg+xml;base64," + base64.b64encode(data).decode()
    return _icons[name]


# Built-in glyphs, drawn in a 24 unit box inside a white rounded frame.
GLYPHS = {
    "server": '<rect x="4" y="4" width="16" height="7" rx="1.5"/><rect x="4" y="13" width="16" height="7" rx="1.5"/>'
              '<path d="M7.5 7.5h.01M7.5 16.5h.01M11 7.5h6M11 16.5h6"/>',
    "users": '<circle cx="9" cy="8" r="3.3"/><path d="M2.8 20c0-3.6 2.8-6 6.2-6s6.2 2.4 6.2 6"/>'
             '<circle cx="16.8" cy="9" r="2.6"/><path d="M16 13.7c3 0 5.2 2.2 5.2 5.3"/>',
    "browser": '<rect x="3" y="4" width="18" height="16" rx="2"/><path d="M3 9h18M6 6.5h.01M8.5 6.5h.01"/>',
    "queue": '<rect x="3" y="8" width="5" height="8" rx="1"/><rect x="9.5" y="8" width="5" height="8" rx="1"/>'
             '<rect x="16" y="8" width="5" height="8" rx="1"/>',
    "db": '<ellipse cx="12" cy="6" rx="7" ry="2.6"/><path d="M5 6v12c0 1.4 3.1 2.6 7 2.6s7-1.2 7-2.6V6"/>'
          '<path d="M5 12c0 1.4 3.1 2.6 7 2.6s7-1.2 7-2.6"/>',
    "file": '<path d="M6 3h8l5 5v13H6z"/><path d="M14 3v5h5M9 13h6M9 17h6"/>',
    "lock": '<rect x="5" y="11" width="14" height="10" rx="2"/><path d="M8 11V8a4 4 0 0 1 8 0v3M12 15v2"/>',
    "clock": '<circle cx="12" cy="12" r="8.5"/><path d="M12 7.5V12l3 2"/>',
    "mail": '<rect x="3" y="6" width="18" height="12" rx="2"/><path d="M3.5 7l8.5 6.5L20.5 7"/>',
    "cloud": '<path d="M7 18.5h10.5a4 4 0 0 0 .6-7.95A6 6 0 0 0 6.6 11.4 3.6 3.6 0 0 0 7 18.5z"/>',
}


def _glyph(x, y, name, size):
    if name == "reload":
        cx, cy, r = x + size / 2, y + size / 2, size * 0.30
        rect(x + 1, y + 1, size - 2, size - 2, "#FFFFFF", GLYPH, 1.6, size * 0.22)
        P.append(f'<path d="M{cx + r},{cy} A{r},{r} 0 1 1 {cx + r * 0.5},{cy - r * 0.87}" fill="none" stroke="{GLYPH}" stroke-width="2.2" stroke-linecap="round"/>')
        P.append(f'<path d="M{cx + r * 0.5 - 1},{cy - r * 0.87 - 5} l6,4.5 l-6.5,3.5 z" fill="{GLYPH}"/>')
        return
    if name not in GLYPHS:
        raise SystemExit(f"unknown glyph: {name}; glyphs: reload, " + ", ".join(GLYPHS))
    rect(x + 1, y + 1, size - 2, size - 2, "#FFFFFF", GLYPH, 1.6, size * 0.22)
    inner = size * 0.64
    k = inner / 24
    o = (size - inner) / 2
    P.append(f'<g transform="translate({x + o:.2f},{y + o:.2f}) scale({k:.4f})" fill="none" stroke="{GLYPH}" '
             f'stroke-width="{2.2 / k:.2f}" stroke-linecap="round" stroke-linejoin="round">{GLYPHS[name]}</g>')


def rect(x, y, w, h, fill="#FFFFFF", stroke="#DCE4ED", sw=1.2, r=12, dash=None, opacity=None):
    ds = f' stroke-dasharray="{dash}"' if dash else ""
    op = f' opacity="{opacity}"' if opacity is not None else ""
    P.append(f'<rect x="{x}" y="{y}" width="{w}" height="{h}" rx="{r}" fill="{fill}" stroke="{stroke}" stroke-width="{sw}"{ds}{op}/>')


def line(x1, y1, x2, y2, color=None, sw=1, dash=None):
    ds = f' stroke-dasharray="{dash}"' if dash else ""
    P.append(f'<line x1="{x1}" y1="{y1}" x2="{x2}" y2="{y2}" stroke="{color or C["line"]}" stroke-width="{sw}"{ds}/>')


def text(x, y, value, size=12, weight=400, color=None, anchor="start", spacing=0, italic=False):
    color = color or C["ink"]
    style = f'font-family:{SANS};font-size:{size}px;font-weight:{weight};fill:{color}'
    if spacing: style += f';letter-spacing:{spacing}px'
    if italic: style += ';font-style:italic'
    P.append(f'<text x="{x}" y="{y}" text-anchor="{anchor}" style="{style}">{esc(value)}</text>')


def image(x, y, name, size=36, opacity=None):
    if name.startswith("glyph:"):
        _glyph(x, y, name[6:], size)
        return
    op = f' opacity="{opacity}"' if opacity is not None else ""
    P.append(f'<image x="{x}" y="{y}" width="{size}" height="{size}" href="{icon_uri(name)}"{op}/>')


def marker(color):
    key = color[1:]
    if any(f'id="a{key}"' in d for d in D): return key
    D.append(f'<marker id="a{key}" viewBox="0 0 10 10" refX="9" refY="5" markerWidth="7" markerHeight="7" orient="auto-start-reverse"><path d="M0 0L10 5L0 10Z" fill="{color}"/></marker>')
    return key


def edge(points, color, label=None, dash=None, width=2, label_at=None, arrows=True, start_arrow=False, box=True, anchor="middle", size=13):
    key = marker(color)
    ds = f' stroke-dasharray="{dash}"' if dash else ""
    me = f' marker-end="url(#a{key})"' if arrows else ""
    if start_arrow:
        me += f' marker-start="url(#a{key})"'
    path = "M " + " L ".join(f"{x},{y}" for x, y in points)
    P.append(f'<path d="{path}" fill="none" stroke="{color}" stroke-width="{width}" stroke-linejoin="round"{ds}{me}/>')
    if label and label_at:
        x, y = label_at
        rows = label if isinstance(label, list) else [label]
        if box:
            wmax = max(tw(r, size, 600) for r in rows) + 14
            bx = x - wmax / 2 if anchor == "middle" else (x - 7 if anchor == "start" else x - wmax + 7)
            rect(bx, y - size - 3, wmax, (size + 4) * len(rows) + 4, C["white"], "none", 0, 4)
        for row in rows:
            text(x, y, row, size, 600, color, anchor)
            y += size + 4


def section(x, y, w, h, title, kicker, fill, stroke, dash=None, kcolor=None):
    rect(x, y, w, h, fill, stroke, 1.4, 14, dash)
    text(x + 18, y + 27, kicker.upper(), 12.5, 700, kcolor or stroke, spacing=1.4)
    text(x + 18, y + 54, title, 21, 700)


def chip(x, y, label, fill, color="#FFFFFF", anchor="start", size=10):
    w = len(label) * size * 0.62 + 22
    if anchor == "end": x = x - w
    rect(x, y, w, 22, fill, "none", 0, 11)
    text(x + w / 2, y + 15, label, size, 700, color, "middle")
    return w


def card(x, y, w, h, title, fill="#FFFFFF", stroke=None, dash=None, note=None, tcolor=None):
    stroke = stroke or C["line"]
    rect(x, y, w, h, fill, stroke, 1.1, 10, dash)
    text(x + 14, y + 24, title.upper(), 12.5, 700, tcolor or C["card_title"], spacing=1.0)
    if note:
        text(x + w - 14, y + 24, note, 14, 500, C["muted"], "end")
    P.append(f'<line x1="{x}" y1="{y+36}" x2="{x+w}" y2="{y+36}" stroke="{stroke}" stroke-width="1"/>')


def node(cx, y, icon_name, label, note=None, size=42, muted=False, lsize=16, nsize=14.5):
    image(cx - size / 2, y, icon_name, size)
    yy = y + size + 19
    for row in label if isinstance(label, list) else [label]:
        text(cx, yy, row, lsize, 650, C["muted"] if muted else C["ink"], "middle")
        yy += lsize + 3
    for row in (note if isinstance(note, list) else [note]) if note else []:
        text(cx, yy, row, nsize, 400, C["muted"], "middle")
        yy += nsize + 3


def mini(cx, y, icon_name, label, note=None):
    node(cx, y, icon_name, label, note, 30, lsize=13, nsize=11.5)


def hexagon(cx, cy, r, fill, stroke=None, sw=0):
    points = " ".join(f"{cx + r*math.cos(math.radians(60*i-30)):.1f},{cy + r*math.sin(math.radians(60*i-30)):.1f}" for i in range(6))
    st = f' stroke="{stroke}" stroke-width="{sw}"' if stroke else ""
    P.append(f'<polygon points="{points}" fill="{fill}"{st}/>')


def pod(cx, y, label, note=None, color=None, stack=0, muted=False, lsize=14, nsize=12, r=23):
    """Kubernetes-style workload hexagon; stack>0 draws replicas behind it."""
    col = color or C["k8s"]
    for i in range(stack, 0, -1):
        hexagon(cx + 7 * i, y + r - 6 * i, r, "#FFFFFF", col, 1.6)
    hexagon(cx, y + r, r, col)
    P.append(f'<rect x="{cx-r*0.43}" y="{y+r*0.61}" width="{r*0.87}" height="{r*0.78}" rx="2" fill="none" stroke="white" stroke-width="2"/>')
    yy = y + 2 * r + 19
    for row in label if isinstance(label, list) else [label]:
        text(cx, yy, row, lsize, 650, C["muted"] if muted else C["ink"], "middle")
        yy += lsize + 3
    for row in (note if isinstance(note, list) else [note]) if note else []:
        text(cx, yy, row, nsize, 400, C["muted"], "middle")
        yy += nsize + 3


def boundary(x, y, w, h, title, note, fill, color, icon=None, dash=None, sw=1.4):
    rect(x, y, w, h, fill, color, sw, 12, dash)
    tx = x + 16
    if icon:
        image(x + 14, y + 12, icon, 25)
        tx = x + 49
    text(tx, y + 30, title, 15, 700)
    if note:
        text(x + w - 18, y + 30, note, 12, 500, C["muted"], "end")


def header(W, title, subtitle, right1):
    P.append(f'<rect width="{W}" height="100%" fill="#FFFFFF"/>')
    text(54, 65, title, 36, 750)
    text(54, 94, subtitle, 15, 400, C["muted"])
    text(W - 54, 65, right1, 13, 650, C["muted"], "end")
    line(54, 111, W - 54, 111)


def legend(x, y, w, h, rows, kicker="Reading the drawing"):
    """rows: (label, kind); kind is a FLOW key (app, carrier, data, deploy), "boundary" or "zone"."""
    section(x, y, w, h, "Legend", kicker, "#FFFFFF", C["legend"])
    ly = y + 84
    for label, kind in rows:
        if kind == "boundary":
            rect(x + 24, ly - 14, 34, 18, C["vpc_fill"], C["vpc"], 1.3, 4)
        elif kind == "zone":
            rect(x + 24, ly - 14, 34, 18, C["zone_fill"], C["zone"], 1.3, 4, "5 4")
        else:
            col = FLOW[kind]
            edge([(x + 24, ly - 4), (x + 62, ly - 4)], col, dash="5 4" if kind == "deploy" else None, width=2)
        text(x + 74, ly, label, 12.5, 500, C["muted"])
        ly += 32


def glyph_pod(cx, y, label, note=None, glyph="box", stack=0, ghost=False, r=23, lsize=16, nsize=14.5, color=None, nstep=None):
    """Workload hexagon with a small white glyph; ghost = may run no replica at all."""
    col = color or C["pod"]
    for i in range(stack, 0, -1):
        hexagon(cx + 7 * i, y + r - 6 * i, r, "#FFFFFF", col, 1.6)
    if ghost:
        pts = " ".join(f"{cx + r*math.cos(math.radians(60*i-30)):.1f},{y + r + r*math.sin(math.radians(60*i-30)):.1f}" for i in range(6))
        P.append(f'<polygon points="{pts}" fill="#FFFFFF" stroke="{col}" stroke-width="2" stroke-dasharray="5 3"/>')
        gc = col
    else:
        hexagon(cx, y + r, r, col)
        gc = "#FFFFFF"
    cy = y + r
    if glyph == "box":
        P.append(f'<rect x="{cx-10}" y="{cy-9}" width="20" height="18" rx="2" fill="none" stroke="{gc}" stroke-width="2"/>')
    elif glyph == "clock":
        P.append(f'<circle cx="{cx}" cy="{cy}" r="10" fill="none" stroke="{gc}" stroke-width="2"/><path d="M{cx},{cy-6}V{cy}L{cx+5},{cy+3}" fill="none" stroke="{gc}" stroke-width="2" stroke-linecap="round"/>')
    elif glyph == "mail":
        P.append(f'<rect x="{cx-11}" y="{cy-8}" width="22" height="16" rx="2" fill="none" stroke="{gc}" stroke-width="2"/><path d="M{cx-11},{cy-7}L{cx},{cy+1}L{cx+11},{cy-7}" fill="none" stroke="{gc}" stroke-width="2"/>')
    elif glyph == "list":
        for yy in (cy - 6, cy, cy + 6):
            P.append(f'<line x1="{cx-9}" y1="{yy}" x2="{cx+9}" y2="{yy}" stroke="{gc}" stroke-width="2"/>')
    elif glyph == "link":
        for lx in (cx - 12, cx - 2):
            P.append(f'<rect x="{lx}" y="{cy-5}" width="14" height="10" rx="5" fill="none" stroke="{gc}" stroke-width="2"/>')
    elif glyph == "shield":
        P.append(f'<path d="M{cx},{cy-10} L{cx+9},{cy-6} V{cy+1} C{cx+9},{cy+6} {cx+4},{cy+9} {cx},{cy+11} C{cx-4},{cy+9} {cx-9},{cy+6} {cx-9},{cy+1} V{cy-6} Z" fill="none" stroke="{gc}" stroke-width="2" stroke-linejoin="round"/>')
    elif glyph == "swap":                                  # two opposed arrows
        P.append(f'<path d="M{cx-10},{cy-4}H{cx+8}M{cx+3},{cy-9}L{cx+9},{cy-4}L{cx+3},{cy+1}" fill="none" stroke="{gc}" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>'
                 f'<path d="M{cx+10},{cy+5}H{cx-8}M{cx-3},{cy}L{cx-9},{cy+5}L{cx-3},{cy+10}" fill="none" stroke="{gc}" stroke-width="2" stroke-linecap="round" stroke-linejoin="round"/>')
    yy = y + 2 * r + lsize + 4
    for row in label if isinstance(label, list) else [label]:
        if row:
            text(cx, yy, row, lsize, 650, anchor="middle")
        yy += lsize + 3
    for row in (note if isinstance(note, list) else [note]) if note else []:
        text(cx, yy, row, nsize, 400, C["muted"], "middle")
        yy += nstep or nsize + 3


def lines_at(x, y, label, note=None, lsize=16, nsize=14.5, anchor="start"):
    """Bold label rows then muted note rows; returns the y after the last row."""
    for r_ in ([label] if isinstance(label, str) else label) or []:
        text(x, y, r_, lsize, 650, anchor=anchor)
        y += lsize + 3
    for n_ in ([note] if isinstance(note, str) else note) if note else []:
        text(x, y, n_, nsize, 400, C["muted"], anchor)
        y += nsize + 4
    return y


def hnode(x, y, icon, label, note=None, size=40, lsize=16, nsize=14.5):
    """Icon left, text right: for narrow columns."""
    image(x, y, icon, size)
    rows = [label] if isinstance(label, str) else label
    notes = ([note] if isinstance(note, str) else note) if note else []
    total = len(rows) * (lsize + 3) + len(notes) * (nsize + 3)
    lines_at(x + size + 12, y + size / 2 - total / 2 + lsize - 1, rows, notes, lsize, nsize)


def hpod(x, y, glyph, label, note=None, size=40, stack=0, lsize=16, nsize=14.5, ghost=False, color=None):
    """Workload hexagon left, text right; returns the hexagon centre."""
    r = size / 2
    glyph_pod(x + r, y, "", None, glyph, stack=stack, r=r, ghost=ghost, color=color)
    rows = [label] if isinstance(label, str) else label
    notes = ([note] if isinstance(note, str) else note) if note else []
    tx = x + size + 12 + 7 * stack
    total = len(rows) * (lsize + 3) + len(notes) * (nsize + 3)
    lines_at(tx, y + r - total / 2 + lsize - 1, rows, notes, lsize, nsize)
    return x + r, y + r


def cluster_box(x, y, w, h, title, note=None, icon=None):
    """Cluster frame; icon is any icon name, e.g. si:kubernetes or Arch_Amazon-Elastic-Kubernetes-Service_64."""
    rect(x, y, w, h, C["cluster_fill"], C["cluster"], 1.8, 12)
    tx = x + 18
    if icon:
        image(x + 16, y + 14, icon, 30)
        tx = x + 56
    text(tx, y + 37, title, 21, 700)
    if note:
        text(x + w - 18, y + 36, note, 14, 500, C["muted"], "end")


def account_box(x, y, w, h, title, icon=None):
    """Account or subscription frame; icon e.g. AWS-Account_32."""
    rect(x, y, w, h, "#FFFFFF", C["account"], 1.8, 18)
    tx = x + 18
    if icon:
        image(x + 18, y + 11, icon, 26)
        tx = x + 54
    text(tx, y + 31, title, 19, 750)


def mono(x, y, value, size=14, color=None, weight=500, anchor="start"):
    style = f'font-family:{MONO};font-size:{size}px;font-weight:{weight};fill:{color or C["ink"]}'
    P.append(f'<text x="{x}" y="{y}" text-anchor="{anchor}" style="{style}" xml:space="preserve">{esc(value)}</text>')


def table(x, y, w, name, cols, note=None, rows=2, color=None):
    """A database table glyph: name bar, column header, empty rows; returns its height."""
    col = color or C["data"]
    h = 34 + 28 + rows * 18 + 6
    rect(x, y, w, h, "#FFFFFF", col, 1.4, 8)
    P.append(f'<path d="M{x},{y+8} a8,8 0 0 1 8,-8 h{w-16} a8,8 0 0 1 8,8 v26 h{-w} z" fill="{col}"/>')
    mono(x + 12, y + 23, name, 15, "#FFFFFF", 600)
    mono(x + 12, y + 54, cols, 13, C["muted"])
    P.append(f'<line x1="{x}" y1="{y+62}" x2="{x+w}" y2="{y+62}" stroke="{C["line"]}" stroke-width="1"/>')
    for k in range(rows):
        yy = y + 62 + 18 * (k + 1)
        P.append(f'<line x1="{x+12}" y1="{yy-6}" x2="{x+w-12}" y2="{yy-6}" stroke="{C["line"]}" stroke-width="5" stroke-linecap="round"/>')
    if note:
        yy = y + h + 20
        for n_ in [note] if isinstance(note, str) else note:
            text(x, yy, n_, 14.5, 400, C["muted"])
            yy += 19
    return h


def badge(cx, cy, n, fill=None, r=12.5):
    P.append(f'<circle cx="{cx}" cy="{cy}" r="{r}" fill="{fill or C["navy"]}"/>')
    text(cx, cy + 5, str(n), 14, 750, "#FFFFFF", "middle")


def zone_band(x, y, w, h, name, note=None):
    rect(x, y, w, h, C["zone_fill"], C["zone"], 1.3, 10, "6 5")
    text(x + 14, y + 24, f"Availability zone {name}", 14, 700, C["navy"])
    if note:
        text(x + w - 14, y + 24, note, 13, 500, C["muted"], "end")


def label(x, y, rows, color, size=13, anchor="middle", box=True):
    """An edge label away from its edge (white backing, one or more rows)."""
    rows = rows if isinstance(rows, list) else [rows]
    if box:
        wmax = max(tw(r_, size, 600) for r_ in rows) + 14
        bx = x - wmax / 2 if anchor == "middle" else (x - 7 if anchor == "start" else x - wmax + 7)
        rect(bx, y - size - 3, wmax, (size + 4) * len(rows) + 4, C["white"], "none", 0, 4)
    for r_ in rows:
        text(x, y, r_, size, 600, color, anchor)
        y += size + 4


def write(name, W, H, out=None):
    """Write <out>/<name>.svg; out defaults to the directory of the running script. Refuses a dash
    character (em or en) and an empty <text>."""
    svg = f'<svg xmlns="http://www.w3.org/2000/svg" width="{W}" height="{H}" viewBox="0 0 {W} {H}"><defs>{"".join(D)}</defs>{"".join(P)}</svg>'
    for bad in (chr(0x2014), chr(0x2013)):
        if bad in svg: raise SystemExit(f"dash character in {name}")
    if re.search(r"<text[^>]*>\s*</text>", svg): raise SystemExit(f"empty <text> in {name}")
    if out is None:
        main = getattr(sys.modules.get("__main__"), "__file__", None)
        out = pathlib.Path(main).resolve().parent if main else pathlib.Path.cwd()
    path = pathlib.Path(out) / f"{name}.svg"
    path.write_text(svg)
    print(f"written {path} {W} x {H}")
    return path
