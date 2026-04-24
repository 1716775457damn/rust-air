"""
Generate rust-air app icons: airplane silhouette on gradient background.
Outputs all sizes required by Tauri v2 into tauri-app/src-tauri/icons/
"""
import math, struct, zlib, os
from PIL import Image, ImageDraw

OUT = r"d:\rust-air\tauri-app\src-tauri\icons"

# ── draw one icon at given size ──────────────────────────────────────────────
def make_icon(size: int) -> Image.Image:
    img = Image.new("RGBA", (size, size), (0, 0, 0, 0))
    d = ImageDraw.Draw(img)

    # rounded-rect background: deep-blue → cyan gradient (manual scanline)
    r = size // 8
    for y in range(size):
        t = y / (size - 1)
        # top: #0f2a4a  bottom: #0ea5e9
        R = int(0x0f + (0x0e - 0x0f) * t)
        G = int(0x2a + (0xa5 - 0x2a) * t)
        B = int(0x4a + (0xe9 - 0x4a) * t)
        d.line([(0, y), (size - 1, y)], fill=(R, G, B, 255))

    # mask to rounded rect
    mask = Image.new("L", (size, size), 0)
    ImageDraw.Draw(mask).rounded_rectangle([0, 0, size - 1, size - 1], radius=r, fill=255)
    img.putalpha(mask)

    # ── airplane polygon (pointing right) ────────────────────────────────────
    s = size
    # scale factor
    def p(x, y):
        return (x * s / 100, y * s / 100)

    # fuselage
    fuselage = [p(15,47), p(55,47), p(72,50), p(55,53), p(15,53)]
    # nose cone
    nose = [p(55,47), p(85,50), p(55,53)]
    # main wing
    wing = [p(25,47), p(45,20), p(52,20), p(40,47)]
    # tail wing
    tail = [p(18,47), p(22,35), p(27,35), p(25,47)]
    # small lower wing
    lwing = [p(25,53), p(40,53), p(52,80), p(45,80)]
    ltail = [p(18,53), p(25,53), p(27,65), p(22,65)]

    white = (255, 255, 255, 240)
    for poly in [fuselage, nose, wing, tail, lwing, ltail]:
        d.polygon(poly, fill=white)

    # subtle glow dot (speed lines)
    for i, offset in enumerate([8, 14, 20]):
        alpha = 180 - i * 50
        cx, cy = int(s * (0.10 - offset * 0.003)), int(s * 0.50)
        r2 = max(1, int(s * 0.012))
        d.ellipse([cx - r2, cy - r2, cx + r2, cy + r2], fill=(255, 255, 255, alpha))

    return img


# ── save helpers ─────────────────────────────────────────────────────────────
def save_png(img: Image.Image, path: str):
    img.save(path, "PNG")
    print(f"  {path}")


def save_ico(images: list, path: str):
    # sizes: 16,24,32,48,64,128,256
    imgs = []
    for sz in [16, 24, 32, 48, 64, 128, 256]:
        imgs.append(make_icon(sz).resize((sz, sz), Image.LANCZOS))
    imgs[0].save(path, format="ICO", sizes=[(i.width, i.height) for i in imgs],
                 append_images=imgs[1:])
    print(f"  {path}")


def save_icns(path: str):
    """Write a minimal .icns with 16/32/64/128/256/512/1024 px entries."""
    TYPES = [
        (b"icp4", 16), (b"icp5", 32), (b"icp6", 64),
        (b"ic07", 128), (b"ic08", 256), (b"ic09", 512), (b"ic10", 1024),
    ]
    import io
    chunks = b""
    for tag, sz in TYPES:
        buf = io.BytesIO()
        make_icon(sz).save(buf, "PNG")
        data = buf.getvalue()
        chunks += tag + struct.pack(">I", len(data) + 8) + data
    total = 8 + len(chunks)
    with open(path, "wb") as f:
        f.write(b"icns" + struct.pack(">I", total) + chunks)
    print(f"  {path}")


# ── generate all ─────────────────────────────────────────────────────────────
print("Generating rust-air icons…")

base = make_icon(1024)

# PNG sizes
for sz, name in [
    (32,  "32x32.png"),
    (128, "128x128.png"),
    (256, "128x128@2x.png"),
    (1024,"icon.png"),
    # Windows Store / UWP
    (30,  "Square30x30Logo.png"),
    (44,  "Square44x44Logo.png"),
    (71,  "Square71x71Logo.png"),
    (89,  "Square89x89Logo.png"),
    (107, "Square107x107Logo.png"),
    (142, "Square142x142Logo.png"),
    (150, "Square150x150Logo.png"),
    (284, "Square284x284Logo.png"),
    (310, "Square310x310Logo.png"),
    (50,  "StoreLogo.png"),
]:
    save_png(make_icon(sz), os.path.join(OUT, name))

save_ico([], os.path.join(OUT, "icon.ico"))
save_icns(os.path.join(OUT, "icon.icns"))

print("Done ✅")
