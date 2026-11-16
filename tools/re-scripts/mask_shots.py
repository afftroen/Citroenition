"""Crop + mask the proxy exit IP out of verification screenshots before they go into the public repo.

    python3 -I mask_shots.py <artifact-dir> <out-dir>
"""

import pathlib
import sys

from PIL import Image, ImageDraw

src = pathlib.Path(sys.argv[1])
dst = pathlib.Path(sys.argv[2])
dst.mkdir(parents=True, exist_ok=True)

# iphey: the IP sits under the IP ADDRESS tile
im = Image.open(src / "iphey.png").convert("RGB")
d = ImageDraw.Draw(im)
d.rectangle((1085, 330, 1250, 350), fill=(55, 58, 64))
d.text((1112, 333), "x.x.x.x (proxy exit)", fill=(150, 150, 150))
im.crop((0, 0, im.width, 700)).save(dst / "verify-iphey.png", optimize=True)

# browserscan: the big IP, the ISP and the DNS-leak resolver
im = Image.open(src / "browserscan.png").convert("RGB")
d = ImageDraw.Draw(im)
d.rectangle((430, 120, 830, 200), fill=(18, 14, 52))
d.text((520, 150), "x.x.x.x  (proxy exit, masked)", fill=(200, 200, 200))
d.rectangle((838, 455, 1000, 480), fill=(22, 24, 58))
d.rectangle((838, 275, 1000, 300), fill=(22, 24, 58))
im.crop((0, 0, im.width, 700)).save(dst / "verify-browserscan.png", optimize=True)

# creepjs: the top of the page has no IP
im = Image.open(src / "creepjs.png").convert("RGB")
im.crop((300, 0, 1070, 900)).save(dst / "verify-creepjs.png", optimize=True)

for p in sorted(dst.glob("verify-*.png")):
    print(p.name, p.stat().st_size)
