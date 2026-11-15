"""python3 -I grep_ctx.py FILE BEFORE AFTER MAXHITS NEEDLE [NEEDLE ...]  -> contexts around each needle, secrets redacted."""
import re
import sys

path, before, after, maxhits = sys.argv[1], int(sys.argv[2]), int(sys.argv[3]), int(sys.argv[4])
src = open(path, encoding="utf-8", errors="replace").read()
SECRET = re.compile(r"(AIza[0-9A-Za-z_\-]{20,}|[A-Za-z0-9+/=_\-]{40,})")


def clean(s: str) -> str:
    return SECRET.sub("«redacted»", s)


for needle in sys.argv[5:]:
    hits = [m.start() for m in re.finditer(re.escape(needle), src)]
    print(f"\n###### {needle} | occurrences: {len(hits)}")
    for h in hits[:maxhits]:
        print(f"--- @{h}")
        print(clean(src[max(0, h - before): h + len(needle) + after]))
