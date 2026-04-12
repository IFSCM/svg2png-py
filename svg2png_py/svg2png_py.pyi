from __future__ import annotations

Transform6 = tuple[float, float, float, float, float, float]


class FontDatabase:
    """Thread-safe font database used when parsing SVG text nodes.

    A single instance can be shared freely across threads and used
    concurrently from a ``concurrent.futures.ThreadPoolExecutor``.
    """

    def __init__(self) -> None: ...

    @staticmethod
    def system() -> FontDatabase:
        """Create a FontDatabase pre-loaded with all system fonts.

        Reuses the process-global font database — subsequent calls in the
        same process pay only an atomic refcount increment, not a full
        font rescan.
        """
        ...

    def load_system_fonts(self) -> None:
        """Add all system fonts to this database."""
        ...

    def load_font_file(self, path: str) -> None:
        """Load a single font file.

        Raises
        ------
        FileNotFoundError
            If ``path`` does not exist on disk.
        RuntimeError
            If the file cannot be parsed as a valid font.
        """
        ...

    def load_fonts_dir(self, dir: str) -> None:
        """Load all fonts found in ``dir`` (non-recursive)."""
        ...

    def __len__(self) -> int: ...
    def __repr__(self) -> str: ...


class RenderOptions:
    """Options that control SVG parsing and rendering."""

    dpi: float
    """DPI used when the SVG does not specify its own resolution. Default: 96.0"""

    font_family: str
    """Default font family for text nodes. Default: ``"Times New Roman"``"""

    font_size: float
    """Default font size in pixels. Default: 12.0"""

    resources_dir: str | None
    """Directory used to resolve relative href/xlink:href references.
    ``None`` falls back to the current working directory."""

    def __init__(
        self,
        dpi: float = 96.0,
        font_family: str = "Times New Roman",
        font_size: float = 12.0,
        resources_dir: str | None = None,
    ) -> None: ...

    def __repr__(self) -> str: ...


def svg_intrinsic_size(
    svg_str: str,
    font_db: FontDatabase,
    options: RenderOptions | None = None,
) -> tuple[int, int]:
    """Return the intrinsic ``(width, height)`` of an SVG in pixels.

    Parses but does not render — cheap. Use this to compute a scaled
    canvas size before calling ``svg_to_png`` with a non-identity transform.
    """
    ...


def svg_to_png(
    svg_str: str,
    font_db: FontDatabase,
    transform: Transform6 | None = None,
    bg_file: str | None = None,
    bg_data: bytes | None = None,
    bg_size: tuple[int, int] | None = None,
    bg_color: tuple[int, int, int, int] | None = None,
    options: RenderOptions | None = None,
) -> bytes:
    """Render an SVG string to PNG bytes.

    Parameters
    ----------
    svg_str:
        SVG document as a UTF-8 string.
    font_db:
        Font database. Use ``FontDatabase.system()`` to auto-load system fonts.
    transform:
        Optional affine transform as ``(a, b, c, d, e, f)`` in row-major
        order — same layout as ``affine.Affine(...)[0:6]``. ``None`` = identity.
    bg_file:
        Path to a PNG file used as the background canvas.
        Mutually exclusive with ``bg_data``.
    bg_data:
        Raw PNG bytes used as the background canvas.
        Mutually exclusive with ``bg_file``.
    bg_size:
        ``(width, height)`` in pixels. Defaults to the SVG's intrinsic size.
        Invalid when ``bg_file`` or ``bg_data`` are set.
    bg_color:
        ``(r, g, b, a)`` fill colour (0–255 each).
        Invalid when ``bg_file`` or ``bg_data`` are set.
    options:
        Additional rendering options.
    """
    ...


def svg_to_png_cached(
    svg_str: str,
    transform: Transform6 | None = None,
    bg_file: str | None = None,
    bg_data: bytes | None = None,
    bg_size: tuple[int, int] | None = None,
    bg_color: tuple[int, int, int, int] | None = None,
    options: RenderOptions | None = None,
) -> bytes:
    """Like ``svg_to_png`` but uses the process-global system font database.

    In a ``ProcessPoolExecutor`` each worker pays the font-scan cost only
    once (first call), making every subsequent call virtually free of
    font-loading overhead.
    """
    ...


def svg_to_rgba(
    svg_str: str,
    font_db: FontDatabase,
    transform: Transform6 | None = None,
    bg_color: tuple[int, int, int, int] | None = None,
    options: RenderOptions | None = None,
) -> tuple[bytes, int, int]:
    """Render an SVG to raw RGBA bytes — no PNG compression.

    ~3–5× faster than ``svg_to_png`` when feeding output directly into
    Pillow, OpenCV, or numpy. Skips the entire zlib/deflate pipeline.

    Returns
    -------
    tuple[bytes, int, int]
        ``(rgba_bytes, width, height)`` where ``rgba_bytes`` is
        ``width × height × 4`` bytes of pre-multiplied RGBA.
        Pass ``mode="RGBa"`` to ``PIL.Image.frombuffer``.
    """
    ...


def svg_to_rgba_cached(
    svg_str: str,
    transform: Transform6 | None = None,
    bg_color: tuple[int, int, int, int] | None = None,
    options: RenderOptions | None = None,
) -> tuple[bytes, int, int]:
    """Like ``svg_to_rgba`` but uses the process-global system font database.

    Fastest possible single-SVG render path when PNG is not required.
    """
    ...


def render_many_par(
    svg_strings: list[str],
    font_db: FontDatabase,
    transform: Transform6 | None = None,
    bg_color: tuple[int, int, int, int] | None = None,
    options: RenderOptions | None = None,
) -> list[bytes]:
    """Render multiple SVGs to PNG bytes in parallel using Rayon.

    All work (parse + render + encode) runs without holding the GIL.
    One Rayon task per SVG; each task is fully independent.

    Prefer this over a Python-side ``ThreadPoolExecutor`` when batch
    size ≥ 4 and SVGs are non-trivial (complex paths, text, or >50 KB).
    """
    ...


def render_many_par_cached(
    svg_strings: list[str],
    transform: Transform6 | None = None,
    bg_color: tuple[int, int, int, int] | None = None,
    options: RenderOptions | None = None,
) -> list[bytes]:
    """Like ``render_many_par`` but uses the process-global system font database.

    Best choice when running inside a ``ProcessPoolExecutor`` with system
    fonts and batches of ≥ 4 non-trivial SVGs.
    """
    ...