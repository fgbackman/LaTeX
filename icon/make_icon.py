"""Draw the Typeset app icon: F E A in Latin Modern (the TeX typeface) --
F and A on the baseline with a slightly smaller E dropped between them, as
in the TeX logo; cream on deep ink, with typesetter's guide lines and an
amber "live" cursor.

    python3 make_icon.py [font.otf] -> icon_1024.png

Renders at 4x and downsamples for clean anti-aliasing.
"""
import sys
from PIL import Image, ImageDraw, ImageFilter, ImageFont

FONT = sys.argv[1] if len(sys.argv) > 1 else "/usr/share/texmf/fonts/opentype/public/lm/lmroman10-bold.otf"
S = 4                      # supersampling factor
W = 1024 * S
INK_TOP, INK_BOT = (38, 62, 104), (13, 24, 44)
CREAM = (246, 238, 220)
AMBER = (240, 170, 62)
GUIDE = (110, 150, 205)

# macOS icon grid: 824 pt body centred in a 1024 canvas.
BODY = 824 * S
OFF = (W - BODY) // 2


def squircle_mask(size, n=5.0):
    """Superellipse |x|^n + |y|^n = 1, close to Apple's continuous-corner shape."""
    m = Image.new("L", (size, size), 0)
    px = m.load()
    r = size / 2
    for y in range(size):
        ny = abs((y + 0.5 - r) / r) ** n
        if ny > 1:
            continue
        half = (1 - ny) ** (1 / n) * r
        x0, x1 = int(round(r - half)), int(round(r + half))
        for x in range(max(0, x0), min(size, x1)):
            px[x, y] = 255
    return m


def vertical_gradient(size, top, bot):
    g = Image.new("RGB", (1, size))
    for y in range(size):
        t = y / (size - 1)
        g.putpixel((0, y), tuple(round(a + (b - a) * t) for a, b in zip(top, bot)))
    return g.resize((size, size))


canvas = Image.new("RGBA", (W, W), (0, 0, 0, 0))
mask = squircle_mask(BODY)

# Soft drop shadow beneath the body.
shadow = Image.new("RGBA", (W, W), (0, 0, 0, 0))
sh = Image.new("RGBA", (BODY, BODY), (0, 0, 0, 110))
shadow.paste(sh, (OFF, OFF + 12 * S), mask)
shadow = shadow.filter(ImageFilter.GaussianBlur(14 * S))
canvas.alpha_composite(shadow)

# Body: ink gradient with a faint top glow.
body = vertical_gradient(BODY, INK_TOP, INK_BOT).convert("RGBA")
canvas.paste(body, (OFF, OFF), mask)

# ---- lettering: F E A ------------------------------------------------------------
# F and A at full size on the baseline; a slightly smaller E between them,
# dropped below the baseline the way the TeX logo drops its E.
big = ImageFont.truetype(FONT, 312 * S)
mid = ImageFont.truetype(FONT, 266 * S)

def ink_box(font, ch):
    return font.getbbox(ch, anchor="ls")   # left, top, right, bottom relative to baseline

fb, eb, ab = ink_box(big, "F"), ink_box(mid, "E"), ink_box(big, "A")
cap = -fb[1]                      # cap height of F/A
drop = round(cap * 0.22)          # TeX-style drop for the E
kern_fe = round(-cap * 0.07)
kern_ea = round(cap * 0.01)
cursor_room = 46 * S

w_f, w_e, w_a = fb[2] - fb[0], eb[2] - eb[0], ab[2] - ab[0]
total = w_f + kern_fe + w_e + kern_ea + w_a + cursor_room
x = (W - total) // 2 - fb[0]
# Centre vertically on the span from the cap line to the E's dropped baseline.
baseline = (W - (cap + drop)) // 2 + cap + 6 * S

d = ImageDraw.Draw(canvas)

# Typesetter's guides: the baseline and the E's dropped baseline.
inset = OFF + 96 * S
for gy, alpha in ((baseline, 110), (baseline + drop, 60)):
    d.line((inset, gy, W - inset, gy), fill=GUIDE + (alpha,), width=2 * S)

letters = []
xf = x
letters.append((big, "F", xf, baseline))
xe = xf + fb[2] + kern_fe - eb[0]
letters.append((mid, "E", xe, baseline + drop))
xa = xe + eb[2] + kern_ea - ab[0]
letters.append((big, "A", xa, baseline))

shade = Image.new("RGBA", (W, W), (0, 0, 0, 0))
sd = ImageDraw.Draw(shade)
for font, ch, lx, ly in letters:
    sd.text((lx, ly + 8 * S), ch, font=font, fill=(0, 0, 0, 120), anchor="ls")
shade = shade.filter(ImageFilter.GaussianBlur(8 * S))
canvas.alpha_composite(shade)

d = ImageDraw.Draw(canvas)
for font, ch, lx, ly in letters:
    d.text((lx, ly), ch, font=font, fill=CREAM, anchor="ls")

# Amber "live" text cursor after the A.
cx = xa + ab[2] + 26 * S
d.rounded_rectangle((cx, baseline - cap - 8 * S, cx + 20 * S, baseline + drop),
                    radius=10 * S, fill=AMBER)

# Clip everything to the body (plus the outer shadow).
out = Image.new("RGBA", (W, W), (0, 0, 0, 0))
out.alpha_composite(shadow)
clipped = Image.new("RGBA", (W, W), (0, 0, 0, 0))
full_mask = Image.new("L", (W, W), 0)
full_mask.paste(mask, (OFF, OFF))
clipped.paste(canvas, (0, 0), full_mask)
out.alpha_composite(clipped)

out.resize((1024, 1024), Image.LANCZOS).save("icon_1024.png")
print("wrote icon_1024.png")
