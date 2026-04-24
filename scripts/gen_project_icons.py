"""
Generate app icons for sysmon, sync-vault, rust-seek.
Each project gets assets/icon.png (1024x1024) + icon.ico.
"""
import os, io, struct
from PIL import Image, ImageDraw, ImageFont

def rounded_bg(size, color_top, color_bot, radius_div=7):
    img = Image.new("RGBA", (size, size), (0,0,0,0))
    d = ImageDraw.Draw(img)
    for y in range(size):
        t = y / (size - 1)
        R = int(color_top[0] + (color_bot[0]-color_top[0])*t)
        G = int(color_top[1] + (color_bot[1]-color_top[1])*t)
        B = int(color_top[2] + (color_bot[2]-color_top[2])*t)
        d.line([(0,y),(size-1,y)], fill=(R,G,B,255))
    mask = Image.new("L",(size,size),0)
    r = size // radius_div
    ImageDraw.Draw(mask).rounded_rectangle([0,0,size-1,size-1], radius=r, fill=255)
    img.putalpha(mask)
    return img

def save_ico(make_fn, path):
    imgs = []
    for sz in [16,24,32,48,64,128,256]:
        imgs.append(make_fn(sz))
    imgs[0].save(path, format="ICO",
                 sizes=[(i.width,i.height) for i in imgs],
                 append_images=imgs[1:])

# ── sysmon: dark green gradient + lightning bolt ─────────────────────────────
def make_sysmon(size):
    img = rounded_bg(size, (5,30,10), (20,90,40))
    d = ImageDraw.Draw(img)
    s = size
    def p(x,y): return (x*s/100, y*s/100)
    # lightning bolt polygon
    bolt = [p(58,12), p(38,52), p(52,52), p(42,88), p(62,48), p(48,48)]
    d.polygon(bolt, fill=(180,255,160,245))
    # glow
    for i,r in enumerate([3,6,9]):
        alpha = 60 - i*18
        cx,cy = int(s*0.50), int(s*0.50)
        d.ellipse([cx-int(s*r/100), cy-int(s*r/100),
                   cx+int(s*r/100), cy+int(s*r/100)],
                  fill=(180,255,160,alpha))
    return img

# ── sync-vault: blue gradient + circular arrows ──────────────────────────────
def make_syncvault(size):
    img = rounded_bg(size, (10,30,80), (20,100,200))
    d = ImageDraw.Draw(img)
    s = size
    cx, cy = s//2, s//2
    R = int(s*0.30)
    w = max(2, int(s*0.09))
    # draw arc ring (simulate with thick arc)
    bbox = [cx-R, cy-R, cx+R, cy+R]
    d.arc(bbox, start=30, end=300, fill=(255,255,255,230), width=w)
    d.arc(bbox, start=210, end=120, fill=(255,255,255,230), width=w)
    # arrowhead top-right
    ax,ay = int(cx + R*0.82), int(cy - R*0.57)
    aw = max(1, int(s*0.07))
    d.polygon([(ax,ay-aw),(ax+aw,ay+aw),(ax-aw,ay+aw)], fill=(255,255,255,230))
    # arrowhead bottom-left
    bx,by = int(cx - R*0.82), int(cy + R*0.57)
    d.polygon([(bx,by+aw),(bx-aw,by-aw),(bx+aw,by-aw)], fill=(255,255,255,230))
    return img

# ── rust-seek: orange gradient + magnifying glass ────────────────────────────
def make_rustseek(size):
    img = rounded_bg(size, (80,30,5), (200,90,10))
    d = ImageDraw.Draw(img)
    s = size
    cx, cy = int(s*0.42), int(s*0.42)
    R = int(s*0.24)
    w = max(2, int(s*0.09))
    # circle
    d.ellipse([cx-R, cy-R, cx+R, cy+R], outline=(255,255,255,230), width=w)
    # handle
    hx1,hy1 = int(cx+R*0.70), int(cy+R*0.70)
    hx2,hy2 = int(cx+R*1.55), int(cy+R*1.55)
    d.line([(hx1,hy1),(hx2,hy2)], fill=(255,255,255,230), width=w)
    return img

projects = [
    ("sysmon",     r"D:\sysmon",     make_sysmon),
    ("sync-vault", r"D:\sync-vault", make_syncvault),
    ("rust-seek",  r"D:\rust-seek",  make_rustseek),
]

for name, root, make_fn in projects:
    assets = os.path.join(root, "assets")
    os.makedirs(assets, exist_ok=True)
    # 1024px PNG
    png_path = os.path.join(assets, "icon.png")
    make_fn(1024).save(png_path, "PNG")
    print(f"  {png_path}")
    # ICO
    ico_path = os.path.join(assets, "icon.ico")
    save_ico(make_fn, ico_path)
    print(f"  {ico_path}")

print("Icons done.")
