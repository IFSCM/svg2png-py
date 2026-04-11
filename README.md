# svg2png-py

Fast, accurate SVG → PNG rendering for Python, powered by the
[`resvg`](https://github.com/linebender/resvg) Rust library.
Built with [PyO3](https://github.com/PyO3/pyo3) and
[Maturin](https://github.com/PyO3/maturin).

Unlike the original `resvg-py`, this binding is **fully thread-safe**: a
`FontDatabase` instance can be shared across threads and called concurrently
from a `ThreadPoolExecutor` without any locking on the Python side.

---

## Features

- Full-fidelity SVG → PNG rasterization via `resvg` / `tiny-skia`
- Thread-safe `FontDatabase` — share one instance across a thread pool
- Affine-free transforms — pass a plain 6-tuple, no extra dependencies
- Batch rendering via `render_many`
- Background canvas: solid colour, existing PNG file, or raw PNG bytes
- Pre-built wheels for Windows, Linux, and macOS (Python ≥ 3.9)

---

## Installation

```bash
pip install svg2png-py
```

---

## Quick start

```python
import svg2png_py

# Load fonts (system fonts + a custom one)
db = svg2png_py.FontDatabase.system()
db.load_font_file("/path/to/MyFont.ttf")

svg = open("diagram.svg", encoding="utf-8").read()

# Render with the SVG's intrinsic size
png_bytes = svg2png_py.svg_to_png(svg, db)
open("diagram.png", "wb").write(png_bytes)
```

---

## API

### `FontDatabase`

```python
db = svg2png_py.FontDatabase()          # empty
db = svg2png_py.FontDatabase.system()   # pre-loaded with system fonts

db.load_system_fonts()                 # add system fonts to an existing db
db.load_font_file("/path/to/font.ttf") # load a single file
db.load_fonts_dir("/path/to/fonts/")   # load all fonts in a directory
len(db)                                # number of loaded font faces
```

---

### `RenderOptions`

```python
opts = svg2png_py.RenderOptions(
    dpi=96.0,                       # default: 96.0
    font_family="Helvetica",        # default: "Times New Roman"
    font_size=12.0,                 # default: 12.0
    resources_dir="/path/to/svgs/", # default: None (cwd)
)
```

---

### `svg_to_png`

```python
png: bytes = svg2png_py.svg_to_png(
    svg_str,            # str — SVG content
    font_db,            # FontDatabase
    transform=None,     # 6-tuple (a,b,c,d,e,f) or None for identity
    bg_file=None,       # str — path to a PNG background
    bg_data=None,       # bytes — raw PNG background
    bg_size=None,       # (w, h) — canvas size (default: SVG intrinsic)
    bg_color=None,      # (r, g, b, a) — fill colour 0-255
    options=None,       # RenderOptions
)
```

---

### `render_many`

```python
pages: list[bytes] = svg2png_py.render_many(
    svg_strings,        # list[str]
    font_db,            # FontDatabase
    transform=None,     # shared transform for all pages
    bg_color=None,      # shared fill colour for all pages
    options=None,       # shared RenderOptions
)
```

---

## Transforms

The `transform` parameter accepts a **6-tuple `(a, b, c, d, e, f)`** in
row-major order.  This is the same layout as `affine.Affine(...)[0:6]`, so
code that previously depended on the `affine` package can simply pass the
slice directly — no library change needed:

```python
# Before (affine required):
# from affine import Affine
# tr = Affine.scale(2)
# data = old_render(tree, tr[0:6])

# After (no extra dependency):
a, b, c, d, e, f = 2, 0, 0, 0, 2, 0   # scale ×2
png = svg2png_py.svg_to_png(svg, db, transform=(a, b, c, d, e, f))
```

Pass `None` (the default) for the identity transform.

---

## Thread-pool example

```python
import svg2png_py
from concurrent.futures import ThreadPoolExecutor

db = svg2png_py.FontDatabase.system()
svgs = [open(f"page{i}.svg").read() for i in range(20)]

def render_one(svg):
    return svg2png_py.svg_to_png(svg, db)   # db shared across threads ✓

with ThreadPoolExecutor() as pool:
    pngs = list(pool.map(render_one, svgs))
```

---

## Credits

This package is a rewrite of
[resvg-py](https://github.com/briceyan/resvg-py) by
[Brice Yan](https://github.com/briceyan), used under the MIT licence.

It wraps the [`resvg`](https://github.com/linebender/resvg) crate (formerly
by RazrFalcon, now maintained by the Linebender project), which uses
[`tiny-skia`](https://github.com/linebender/tiny-skia) for rasterization and
[`fontdb`](https://github.com/RazrFalcon/fontdb) for font resolution.

---

## License

MIT
