"""Baseline: what the original fonter.py did (read whole file, fontTools getBestCmap)."""
import io, os, sys, time
from fontTools.ttLib import TTFont

dirs = [os.path.join(os.environ.get("WINDIR", r"C:\Windows"), "Fonts"),
        os.path.join(os.environ.get("LOCALAPPDATA", ""), r"Microsoft\Windows\Fonts")]
files = [os.path.join(d, f) for d in dirs if os.path.isdir(d) for f in os.listdir(d)
         if f.lower().endswith((".ttf", ".otf", ".ttc"))]
limit = int(sys.argv[1]) if len(sys.argv) > 1 else len(files)
step = max(1, len(files) // limit)
sample = files[::step][:limit]
t = time.perf_counter()
ok = 0
for p in sample:
    try:
        with open(p, "rb") as f:
            data = f.read()
        font = TTFont(io.BytesIO(data), fontNumber=0, lazy=True)
        cmap = font.getBestCmap() or {}
        font.close()
        ok += 1
    except Exception:
        pass
dt = time.perf_counter() - t
print(f"fontTools: {len(sample)} files in {dt*1000:.0f} ms ({dt/len(sample)*1000:.2f} ms/file) -> "
      f"~{dt/len(sample)*len(files):.1f} s for all {len(files)} files")
