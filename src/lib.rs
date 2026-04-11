// svg2png-py — thread-safe Python bindings for resvg SVG → PNG rendering
//
// Original resvg-py by Brice Yan (https://github.com/briceyan/resvg-py), MIT licence.
// Rewritten for thread safety and affine-free transforms.

use pyo3::exceptions::{PyFileNotFoundError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use std::path::Path;
use std::sync::{Arc, RwLock};
use tiny_skia::{Color, Pixmap, Transform};

// ---------------------------------------------------------------------------
// FontDatabase
// ---------------------------------------------------------------------------

/// Manages the font database used when parsing SVG text nodes.
///
/// Thread-safe: a single `FontDatabase` instance may be shared across threads
/// and used concurrently from a `ThreadPoolExecutor`.
///
/// Internally uses `Arc<RwLock<Arc<fontdb::Database>>>` so that:
/// - render calls pay only an atomic refcount increment (cheap snapshot)
/// - font mutations (load_font_file etc.) do a copy-on-write clone of the DB,
///   meaning the full clone cost is paid only when fonts are added, not on
///   every render.
#[pyclass(frozen)]
#[derive(Clone)]
pub struct FontDatabase {
    inner: Arc<RwLock<Arc<usvg::fontdb::Database>>>,
}

impl FontDatabase {
    /// Return a cheap `Arc` snapshot of the current database.
    /// Callers get a stable view for the duration of a render without
    /// blocking writers any longer than the refcount bump.
    fn snapshot(&self) -> Arc<usvg::fontdb::Database> {
        self.inner.read().unwrap().clone()
    }
}

#[pymethods]
impl FontDatabase {
    /// Create an empty font database.
    #[new]
    pub fn new() -> Self {
        FontDatabase {
            inner: Arc::new(RwLock::new(Arc::new(usvg::fontdb::Database::new()))),
        }
    }

    /// Create a font database pre-loaded with all system fonts.
    #[staticmethod]
    pub fn system() -> Self {
        let db = FontDatabase::new();
        db.load_system_fonts();
        db
    }

    /// Add all system fonts to this database.
    pub fn load_system_fonts(&self) {
        let mut guard = self.inner.write().unwrap();
        // Copy-on-write: clone the DB, mutate, store new Arc.
        let mut db = (*guard).as_ref().clone();
        db.load_system_fonts();
        *guard = Arc::new(db);
    }

    /// Load a single font file.
    ///
    /// Raises `FileNotFoundError` if the path does not exist,
    /// `RuntimeError` if the file cannot be parsed as a font.
    pub fn load_font_file(&self, path: &str) -> PyResult<()> {
        if !Path::new(path).exists() {
            return Err(PyFileNotFoundError::new_err(format!(
                "font file not found: {path}"
            )));
        }
        let mut guard = self.inner.write().unwrap();
        let mut db = (*guard).as_ref().clone();
        db.load_font_file(path)
            .map_err(|e| PyRuntimeError::new_err(format!("failed to load font '{path}': {e}")))?;
        *guard = Arc::new(db);
        Ok(())
    }

    /// Load all font files found in a directory (non-recursive).
    pub fn load_fonts_dir(&self, dir: &str) {
        let mut guard = self.inner.write().unwrap();
        let mut db = (*guard).as_ref().clone();
        db.load_fonts_dir(dir);
        *guard = Arc::new(db);
    }

    /// Number of font faces currently loaded.
    pub fn __len__(&self) -> usize {
        self.inner.read().unwrap().len()
    }

    pub fn __repr__(&self) -> String {
        format!("FontDatabase(faces={})", self.__len__())
    }
}

// ---------------------------------------------------------------------------
// RenderOptions
// ---------------------------------------------------------------------------

/// Options that control how an SVG is parsed and rendered.
#[pyclass]
#[derive(Clone)]
pub struct RenderOptions {
    /// Default DPI used when the SVG does not specify one. (default: 96.0)
    #[pyo3(get, set)]
    pub dpi: f32,

    /// Default font family for text nodes. (default: "Times New Roman")
    #[pyo3(get, set)]
    pub font_family: String,

    /// Default font size in pixels. (default: 12.0)
    #[pyo3(get, set)]
    pub font_size: f32,

    /// Path used to resolve relative `href` / `xlink:href` references.
    /// When `None` the current working directory is used.
    #[pyo3(get, set)]
    pub resources_dir: Option<String>,
}

#[pymethods]
impl RenderOptions {
    #[new]
    #[pyo3(signature = (
        dpi = 96.0,
        font_family = String::from("Times New Roman"),
        font_size = 12.0,
        resources_dir = None,
    ))]
    pub fn new(
        dpi: f32,
        font_family: String,
        font_size: f32,
        resources_dir: Option<String>,
    ) -> Self {
        RenderOptions {
            dpi,
            font_family,
            font_size,
            resources_dir,
        }
    }

    pub fn __repr__(&self) -> String {
        format!(
            "RenderOptions(dpi={}, font_family={:?}, font_size={}, resources_dir={:?})",
            self.dpi, self.font_family, self.font_size, self.resources_dir
        )
    }
}

// ---------------------------------------------------------------------------
// Transform helpers
// ---------------------------------------------------------------------------

/// Accept an optional 6-tuple `(a, b, c, d, e, f)` in **row-major** order.
///
/// This matches the convention used by the `affine` library's `Affine[0:6]`
/// slice, so existing code that was already using `affine` continues to work
/// without the dependency — just pass the tuple directly.
/// Passing `None` uses the identity transform.
fn transform_from_tuple(t: Option<(f64, f64, f64, f64, f64, f64)>) -> Transform {
    match t {
        None => Transform::identity(),
        Some((a, b, c, d, e, f)) => {
            // tiny-skia from_row: (sx, ky, kx, sy, tx, ty)
            // affine row-major:   (a=sx, b=kx, c=tx, d=ky, e=sy, f=ty)
            Transform::from_row(a as f32, d as f32, b as f32, e as f32, c as f32, f as f32)
        }
    }
}

// ---------------------------------------------------------------------------
// Background canvas
// ---------------------------------------------------------------------------

enum Background {
    /// SVG intrinsic size, transparent fill.
    FromSvg,
    /// Explicit pixel size, transparent fill.
    Sized(u32, u32),
    /// Explicit pixel size, solid RGBA fill.
    Colored(u32, u32, u8, u8, u8, u8),
    /// Pre-existing PNG loaded from a file path.
    File(String),
    /// Pre-existing PNG loaded from raw bytes.
    Data(Vec<u8>),
}

fn make_pixmap(bg: Background, tree_size: tiny_skia::IntSize) -> Result<Pixmap, String> {
    match bg {
        Background::FromSvg => Pixmap::new(tree_size.width(), tree_size.height())
            .ok_or_else(|| "failed to allocate pixmap".to_string()),

        Background::Sized(w, h) => {
            Pixmap::new(w, h).ok_or_else(|| "failed to allocate pixmap".to_string())
        }

        Background::Colored(w, h, r, g, b, a) => {
            let mut pm =
                Pixmap::new(w, h).ok_or_else(|| "failed to allocate pixmap".to_string())?;
            pm.fill(Color::from_rgba8(r, g, b, a));
            Ok(pm)
        }

        Background::File(path) => Pixmap::load_png(&path)
            .map_err(|e| format!("failed to load PNG '{path}': {e}")),

        Background::Data(bytes) => Pixmap::decode_png(&bytes)
            .map_err(|e| format!("failed to decode PNG bytes: {e}")),
    }
}

// ---------------------------------------------------------------------------
// SVG parsing (GIL-free safe — all pure Rust)
// ---------------------------------------------------------------------------

/// Build `usvg::Options` from an optional `RenderOptions` and a DB snapshot.
/// Takes ownership of the `Arc<fontdb::Database>` snapshot.
fn build_usvg_options(
    options: Option<&RenderOptions>,
    fontdb: Arc<usvg::fontdb::Database>,
) -> usvg::Options<'static> {
    let mut opts = usvg::Options::default();
    opts.fontdb = fontdb;
    if let Some(o) = options {
        opts.dpi = o.dpi;
        opts.font_family = o.font_family.clone();
        opts.font_size = o.font_size;
        opts.resources_dir = o.resources_dir.as_deref().map(std::path::PathBuf::from);
    }
    opts
}

fn parse_svg_inner(
    svg: &str,
    opts: &usvg::Options<'_>,
) -> Result<usvg::Tree, String> {
    usvg::Tree::from_str(svg, opts).map_err(|e| format!("SVG parse error: {e}"))
}

// ---------------------------------------------------------------------------
// Public Python functions
// ---------------------------------------------------------------------------

/// Return the intrinsic ``(width, height)`` of an SVG in pixels.
///
/// Parses the SVG but does not render — cheap, and the correct way to
/// compute a scaled canvas size before calling ``svg_to_png`` with a
/// non-identity transform.
#[pyfunction]
pub fn svg_intrinsic_size(
    py: Python<'_>,
    svg_str: String,
    font_db: &FontDatabase,
    options: Option<&RenderOptions>,
) -> PyResult<(u32, u32)> {
    let fontdb = font_db.snapshot();
    let cloned_options = options.cloned();

    py.allow_threads(move || -> Result<(u32, u32), String> {
        let opts = build_usvg_options(cloned_options.as_ref(), fontdb);
        let tree = parse_svg_inner(&svg_str, &opts)?;  // already returns String error
        let sz = tree.size().to_int_size();
        Ok((sz.width(), sz.height()))
    })
    .map_err(|e| PyValueError::new_err(e))  // convert to PyErr after re-acquiring GIL
}

/// Render an SVG string to PNG bytes.
///
/// Parameters
/// ----------
/// svg_str:
///     SVG document as a UTF-8 string.
/// font_db:
///     Font database. Use `FontDatabase.system()` to auto-load system fonts.
/// transform:
///     Optional affine transform as a 6-tuple ``(a, b, c, d, e, f)``
///     in **row-major** order. Pass ``None`` (default) for identity.
/// bg_file:
///     Path to a PNG file used as the background canvas.
///     Mutually exclusive with ``bg_data``.
/// bg_data:
///     Raw PNG bytes used as the background canvas.
///     Mutually exclusive with ``bg_file``.
/// bg_size:
///     ``(width, height)`` in pixels. Defaults to the SVG's intrinsic size.
///     Invalid when ``bg_file`` or ``bg_data`` are set.
/// bg_color:
///     ``(r, g, b, a)`` fill color for the canvas (0–255 each).
///     Invalid when ``bg_file`` or ``bg_data`` are set.
/// options:
///     Additional rendering options (DPI, default font, etc.).
///
/// Returns
/// -------
/// bytes
///     Raw PNG image data.
#[pyfunction]
#[pyo3(signature = (
    svg_str,
    font_db,
    transform = None,
    bg_file = None,
    bg_data = None,
    bg_size = None,
    bg_color = None,
    options = None,
))]
pub fn svg_to_png<'py>(
    py: Python<'py>,
    svg_str: String,
    font_db: &FontDatabase,
    transform: Option<(f64, f64, f64, f64, f64, f64)>,
    bg_file: Option<String>,
    bg_data: Option<Vec<u8>>,
    bg_size: Option<(u32, u32)>,
    bg_color: Option<(u8, u8, u8, u8)>,
    options: Option<&RenderOptions>,
) -> PyResult<Bound<'py, PyBytes>> {
    // Validate mutually-exclusive background args before releasing the GIL.
    if bg_file.is_some() && bg_data.is_some() {
        return Err(PyValueError::new_err(
            "at most one of bg_file or bg_data may be specified",
        ));
    }
    if bg_file.is_some() && (bg_color.is_some() || bg_size.is_some()) {
        return Err(PyValueError::new_err(
            "bg_size and bg_color are not valid when bg_file is set",
        ));
    }
    if bg_data.is_some() && (bg_color.is_some() || bg_size.is_some()) {
        return Err(PyValueError::new_err(
            "bg_size and bg_color are not valid when bg_data is set",
        ));
    }

    // Snapshot the font DB (cheap Arc clone) before releasing the GIL.
    let fontdb = font_db.snapshot();
    let cloned_options = options.cloned();

    // All heavy Rust work happens without holding the GIL.
    let png = py
        .allow_threads(move || -> Result<Vec<u8>, String> {
            let opts = build_usvg_options(cloned_options.as_ref(), fontdb);
            let tree = parse_svg_inner(&svg_str, &opts)?;
            let tree_size = tree.size().to_int_size();

            let bg = if let Some(path) = bg_file {
                Background::File(path)
            } else if let Some(data) = bg_data {
                Background::Data(data)
            } else {
                let (w, h) = bg_size
                    .map(|(w, h)| (w, h))
                    .unwrap_or((tree_size.width(), tree_size.height()));
                match bg_color {
                    Some((r, g, b, a)) => Background::Colored(w, h, r, g, b, a),
                    None => Background::Sized(w, h),
                }
            };

            let mut pixmap = make_pixmap(bg, tree_size)?;
            let tr = transform_from_tuple(transform);
            resvg::render(&tree, tr, &mut pixmap.as_mut());

            pixmap
                .encode_png()
                .map_err(|e| format!("PNG encoding error: {e}"))
        })
        .map_err(PyRuntimeError::new_err)?;

    Ok(PyBytes::new_bound(py, &png))
}

/// Render multiple SVG strings to PNG bytes in one call.
///
/// All heavy work (parse + render + encode) runs without holding the GIL,
/// so the calling thread does not block other Python threads for the
/// duration of the batch.
///
/// Parameters
/// ----------
/// svg_strings:
///     List of SVG documents as UTF-8 strings.
/// font_db:
///     Font database.
/// transform:
///     Optional shared transform applied to every page.
/// bg_color:
///     Optional ``(r, g, b, a)`` background fill applied to every page.
/// options:
///     Optional render options shared across all pages.
///
/// Returns
/// -------
/// list[bytes]
///     One PNG byte-string per input SVG, in the same order.
#[pyfunction]
#[pyo3(signature = (svg_strings, font_db, transform = None, bg_color = None, options = None))]
pub fn render_many<'py>(
    py: Python<'py>,
    svg_strings: Vec<String>,
    font_db: &FontDatabase,
    transform: Option<(f64, f64, f64, f64, f64, f64)>,
    bg_color: Option<(u8, u8, u8, u8)>,
    options: Option<&RenderOptions>,
) -> PyResult<Vec<Bound<'py, PyBytes>>> {
    let fontdb = font_db.snapshot();
    let cloned_options = options.cloned();

    let blobs: Vec<Vec<u8>> = py
        .allow_threads(move || -> Result<Vec<Vec<u8>>, String> {
            let tr = transform_from_tuple(transform);
            let base_opts = build_usvg_options(cloned_options.as_ref(), fontdb);

            svg_strings.iter().enumerate().map(|(i, svg_str)| {
                    // Only clone the Arc pointer, not the strings
                    let tree = parse_svg_inner(svg_str, &base_opts)
                        .map_err(|e| format!("page {i}: {e}"))?;

                    let sz = tree.size().to_int_size();
                    let mut pixmap = match bg_color {
                        Some((r, g, b, a)) => make_pixmap(
                            Background::Colored(sz.width(), sz.height(), r, g, b, a),
                            sz,
                        ),
                        None => make_pixmap(Background::FromSvg, sz),
                    }
                    .map_err(|e| format!("page {i}: {e}"))?;

                    resvg::render(&tree, tr, &mut pixmap.as_mut());
                    pixmap
                        .encode_png()
                        .map_err(|e| format!("page {i} PNG encode: {e}"))
                })
                .collect()
        })
        .map_err(PyRuntimeError::new_err)?;  // <-- semicolon here, blobs is now bound

    // Re-acquire GIL only to wrap the finished blobs in PyBytes.
    blobs
        .iter()
        .map(|b| Ok(PyBytes::new_bound(py, b)))
        .collect()
}

// ---------------------------------------------------------------------------
// Module
// ---------------------------------------------------------------------------

#[pymodule]
fn svg2png_py(m: &Bound<'_, PyModule>) -> PyResult<()> {
    m.add_class::<FontDatabase>()?;
    m.add_class::<RenderOptions>()?;
    m.add_function(wrap_pyfunction!(svg_to_png, m)?)?;
    m.add_function(wrap_pyfunction!(render_many, m)?)?;
    m.add_function(wrap_pyfunction!(svg_intrinsic_size, m)?)?;
    Ok(())
}