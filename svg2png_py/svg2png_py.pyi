from __future__ import annotations

Transform6 = tuple[float, float, float, float, float, float]


class FontDatabase:
    """Thread-safe font database used when parsing SVG text nodes."""

    def __init__(self) -> None: ...

    @staticmethod
    def system() -> FontDatabase:
        """Create a FontDatabase pre-loaded with all system fonts."""
        ...

    def load_system_fonts(self) -> None: ...
    def load_font_file(self, path: str) -> None: ...
    def load_fonts_dir(self, dir: str) -> None: ...
    def __len__(self) -> int: ...
    def __repr__(self) -> str: ...


class RenderOptions:
    dpi: float
    font_family: str
    font_size: float
    resources_dir: str | None

    def __init__(
        self,
        dpi: float = 96.0,
        font_family: str = "Times New Roman",
        font_size: float = 12.0,
        resources_dir: str | None = None,
    ) -> None: ...
    def __repr__(self) -> str: ...


def svg_to_png(
    svg_str: str,
    font_db: FontDatabase,
    transform: Transform6 | None = None,
    bg_file: str | None = None,
    bg_data: bytes | None = None,
    bg_size: tuple[int, int] | None = None,
    bg_color: tuple[int, int, int, int] | None = None,
    options: RenderOptions | None = None,
) -> bytes: ...


def render_many(
    svg_strings: list[str],
    font_db: FontDatabase,
    transform: Transform6 | None = None,
    bg_color: tuple[int, int, int, int] | None = None,
    options: RenderOptions | None = None,
) -> list[bytes]: ...


def svg_intrinsic_size(
    svg_str: str,
    font_db: FontDatabase,
) -> tuple[int, int]:
    """Return the intrinsic ``(width, height)`` of an SVG in pixels.

    Parses the SVG using the given font database but does not render —
    this is cheap and is the correct way to compute a scaled canvas size
    before calling ``svg_to_png`` with a non-identity transform.
    """
    ...