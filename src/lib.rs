// svg2png-py — thread-safe Python bindings for resvg SVG → PNG rendering
//
// Original resvg-py by Brice Yan (https://github.com/briceyan/resvg-py), MIT licence.
// Rewritten for thread safety, affine-free transforms, and maximum performance.
//
// Optimisations applied:
//   1. OnceLock process-global system FontDatabase — font loading paid once per worker
//   2. Rayon parallel iteration in render_many_par — full multi-core within one GIL release
//   3. svg_to_rgba — raw RGBA output path, skips zlib/deflate entirely
//   4. pyo3_disable_reference_pool flag (set in .cargo/config.toml) removes GIL sync overhead
//   5. LTO + codegen-units = 1 in Cargo.toml for cross-crate inlining

use pyo3::exceptions::{PyFileNotFoundError, PyRuntimeError, PyValueError};
use pyo3::prelude::*;
use pyo3::types::PyBytes;
use rayon::prelude::*;
use std::path::Path;
use std::sync::{Arc, OnceLock, RwLock};
use tiny_skia::{Color, Pixmap, Transform};


// ---------------------------------------------------------------------------
// Process-global system font database (Optimisation 1)
// ---------------------------------------------------------------------------

static SYSTEM_FONTDB: OnceLock<Arc<usvg::fontdb::Database>> = OnceLock::new();

fn get_system_fontdb() -> Arc<usvg::fontdb::Database> {
    SYSTEM_FONTDB
        .get_or_init(|| {
            let mut db = usvg::fontdb::Database::new();
            db.load_system_fonts();
            Arc::new(db)
        })
        .clone()
}


// ---------------------------------------------------------------------------
// FontDatabase
// ---------------------------------------------------------------------------

#[pyclass(frozen)]
#[derive(Clone)]
pub struct FontDatabase {
    inner: Arc<RwLock<Arc<usvg::fontdb::Database>>>,
}

impl FontDatabase {
    fn snapshot(&self) -> Arc<usvg::fontdb::Database> {
        self.inner.read().unwrap().clone()
    }
}

#[pymethods]
impl FontDatabase {
    #[new]
    pub fn new() -> Self {
        FontDatabase {
            inner: Arc::new(RwLock::new(Arc::new(usvg::fontdb::Database::new()))),
        }
    }

    #[staticmethod]
    pub fn system() -> Self {
        FontDatabase {
            inner: Arc::new(RwLock::new(get_system_fontdb())),
        }
    }

    pub fn load_system_fonts(&self) {
        let mut guard = self.inner.write().unwrap();
        let mut db = (*guard).as_ref().clone();
        db.load_system_fonts();
        *guard = Arc::new(db);
    }

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

    pub fn load_fonts_dir(&self, dir: &str) {
        let mut guard = self.inner.write().unwrap();
        let mut db = (*guard).as_ref().clone();
        db.load_fonts_dir(dir);
        *guard = Arc::new(db);
    }

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

#[pyclass]
#[derive(Clone)]
pub struct RenderOptions {
    #[pyo3(get, set)]
    pub dpi: f32,
    #[pyo3(get, set)]
    pub font_family: String,
    #[pyo3(get, set)]
    pub font_size: f32,
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
        RenderOptions { dpi, font_family, font_size, resources_dir }
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

#[inline(always)]
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
    FromSvg,
    Sized(u32, u32),
    Colored(u32, u32, u8, u8, u8, u8),
    File(String),
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
// SVG parsing helpers
// ---------------------------------------------------------------------------

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

#[inline(always)]
fn parse_svg_inner(svg: &str, opts: &usvg::Options<'_>) -> Result<usvg::Tree, String> {
    usvg::Tree::from_str(svg, opts).map_err(|e| format!("SVG parse error: {e}"))
}


// ---------------------------------------------------------------------------
// Shared render cores
// ---------------------------------------------------------------------------

fn render_to_png(
    svg_str: String,
    fontdb: Arc<usvg::fontdb::Database>,
    transform: Option<(f64, f64, f64, f64, f64, f64)>,
    bg_file: Option<String>,
    bg_data: Option<Vec<u8>>,
    bg_size: Option<(u32, u32)>,
    bg_color: Option<(u8, u8, u8, u8)>,
    options: Option<RenderOptions>,
) -> Result<Vec<u8>, String> {
    let opts = build_usvg_options(options.as_ref(), fontdb);
    let tree = parse_svg_inner(&svg_str, &opts)?;
    let tree_size = tree.size().to_int_size();

    let bg = if let Some(path) = bg_file {
        Background::File(path)
    } else if let Some(data) = bg_data {
        Background::Data(data)
    } else {
        let (w, h) = bg_size.unwrap_or((tree_size.width(), tree_size.height()));
        match bg_color {
            Some((r, g, b, a)) => Background::Colored(w, h, r, g, b, a),
            None => Background::Sized(w, h),
        }
    };

    let mut pixmap = make_pixmap(bg, tree_size)?;
    let tr = transform_from_tuple(transform);
    resvg::render(&tree, tr, &mut pixmap.as_mut());
    pixmap.encode_png().map_err(|e| format!("PNG encoding error: {e}"))
}

fn render_to_rgba(
    svg_str: String,
    fontdb: Arc<usvg::fontdb::Database>,
    transform: Option<(f64, f64, f64, f64, f64, f64)>,
    bg_color: Option<(u8, u8, u8, u8)>,
    options: Option<RenderOptions>,
) -> Result<(Vec<u8>, u32, u32), String> {
    let opts = build_usvg_options(options.as_ref(), fontdb);
    let tree = parse_svg_inner(&svg_str, &opts)?;
    let sz = tree.size().to_int_size();
    let (w, h) = (sz.width(), sz.height());

    let mut pixmap = make_pixmap(
        match bg_color {
            Some((r, g, b, a)) => Background::Colored(w, h, r, g, b, a),
            None => Background::FromSvg,
        },
        sz,
    )?;

    let tr = transform_from_tuple(transform);
    resvg::render(&tree, tr, &mut pixmap.as_mut());
    Ok((pixmap.take(), w, h))
}


// ---------------------------------------------------------------------------
// svg_intrinsic_size
// ---------------------------------------------------------------------------

/// Return the intrinsic ``(width, height)`` of an SVG in pixels.
/// Parses but does not render — use this to compute scaled canvas sizes
/// before calling ``svg_to_png`` with a non-identity transform.
#[pyfunction]
#[pyo3(signature = (svg_str, font_db, options = None))]
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
        let tree = parse_svg_inner(&svg_str, &opts)?;
        let sz = tree.size().to_int_size();
        Ok((sz.width(), sz.height()))
    })
    .map_err(|e| PyValueError::new_err(e))
}


// ---------------------------------------------------------------------------
// svg_to_png
// ---------------------------------------------------------------------------

/// Render an SVG string to PNG bytes.
#[pyfunction]
#[pyo3(signature = (
    svg_str, font_db,
    transform = None, bg_file = None, bg_data = None,
    bg_size = None, bg_color = None, options = None,
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

    let fontdb = font_db.snapshot();
    let cloned_options = options.cloned();

    let png = py
        .allow_threads(move || {
            render_to_png(
                svg_str, fontdb, transform,
                bg_file, bg_data, bg_size, bg_color,
                cloned_options,
            )
        })
        .map_err(PyRuntimeError::new_err)?;

    Ok(PyBytes::new_bound(py, &png))
}


// ---------------------------------------------------------------------------
// svg_to_png_cached
// ---------------------------------------------------------------------------

/// Like ``svg_to_png`` but uses the process-global system font database.
/// In a ``ProcessPoolExecutor`` each worker pays the font-scan cost only once.
#[pyfunction]
#[pyo3(signature = (
    svg_str,
    transform = None, bg_file = None, bg_data = None,
    bg_size = None, bg_color = None, options = None,
))]
pub fn svg_to_png_cached<'py>(
    py: Python<'py>,
    svg_str: String,
    transform: Option<(f64, f64, f64, f64, f64, f64)>,
    bg_file: Option<String>,
    bg_data: Option<Vec<u8>>,
    bg_size: Option<(u32, u32)>,
    bg_color: Option<(u8, u8, u8, u8)>,
    options: Option<&RenderOptions>,
) -> PyResult<Bound<'py, PyBytes>> {
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

    let fontdb = get_system_fontdb();
    let cloned_options = options.cloned();

    let png = py
        .allow_threads(move || {
            render_to_png(
                svg_str, fontdb, transform,
                bg_file, bg_data, bg_size, bg_color,
                cloned_options,
            )
        })
        .map_err(PyRuntimeError::new_err)?;

    Ok(PyBytes::new_bound(py, &png))
}


// ---------------------------------------------------------------------------
// svg_to_rgba
// ---------------------------------------------------------------------------

/// Render an SVG to raw RGBA bytes — no PNG compression.
///
/// ~3–5× faster than ``svg_to_png`` when feeding output directly into
/// Pillow, OpenCV, or numpy. Skips the entire zlib/deflate pipeline.
///
/// Returns ``(rgba_bytes, width, height)``.
/// Pass ``mode="RGBa"`` (pre-multiplied) to ``PIL.Image.frombuffer``.
#[pyfunction]
#[pyo3(signature = (svg_str, font_db, transform = None, bg_color = None, options = None))]
pub fn svg_to_rgba<'py>(
    py: Python<'py>,
    svg_str: String,
    font_db: &FontDatabase,
    transform: Option<(f64, f64, f64, f64, f64, f64)>,
    bg_color: Option<(u8, u8, u8, u8)>,
    options: Option<&RenderOptions>,
) -> PyResult<(Bound<'py, PyBytes>, u32, u32)> {
    let fontdb = font_db.snapshot();
    let cloned_options = options.cloned();

    let (raw, w, h) = py
        .allow_threads(move || {
            render_to_rgba(svg_str, fontdb, transform, bg_color, cloned_options)
        })
        .map_err(PyRuntimeError::new_err)?;

    Ok((PyBytes::new_bound(py, &raw), w, h))
}


// ---------------------------------------------------------------------------
// svg_to_rgba_cached
// ---------------------------------------------------------------------------

/// Like ``svg_to_rgba`` but uses the process-global system font database.
/// Fastest possible single-SVG render path when PNG is not required.
#[pyfunction]
#[pyo3(signature = (svg_str, transform = None, bg_color = None, options = None))]
pub fn svg_to_rgba_cached<'py>(
    py: Python<'py>,
    svg_str: String,
    transform: Option<(f64, f64, f64, f64, f64, f64)>,
    bg_color: Option<(u8, u8, u8, u8)>,
    options: Option<&RenderOptions>,
) -> PyResult<(Bound<'py, PyBytes>, u32, u32)> {
    let fontdb = get_system_fontdb();
    let cloned_options = options.cloned();

    let (raw, w, h) = py
        .allow_threads(move || {
            render_to_rgba(svg_str, fontdb, transform, bg_color, cloned_options)
        })
        .map_err(PyRuntimeError::new_err)?;

    Ok((PyBytes::new_bound(py, &raw), w, h))
}


// ---------------------------------------------------------------------------
// render_many_par
// ---------------------------------------------------------------------------

/// Render multiple SVGs to PNG bytes in parallel using Rayon.
/// All work runs without holding the GIL.
#[pyfunction]
#[pyo3(signature = (svg_strings, font_db, transform = None, bg_color = None, options = None))]
pub fn render_many_par<'py>(
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

            svg_strings
                .par_iter()
                .enumerate()
                .map(|(i, svg_str)| {
                    let tree = parse_svg_inner(svg_str, &base_opts)
                        .map_err(|e| format!("page {i}: {e}"))?;
                    let sz = tree.size().to_int_size();
                    let (w, h) = (sz.width(), sz.height());

                    let mut pixmap = make_pixmap(
                        match bg_color {
                            Some((r, g, b, a)) => Background::Colored(w, h, r, g, b, a),
                            None => Background::FromSvg,
                        },
                        sz,
                    )
                    .map_err(|e| format!("page {i}: {e}"))?;

                    resvg::render(&tree, tr, &mut pixmap.as_mut());
                    pixmap
                        .encode_png()
                        .map_err(|e| format!("page {i} PNG encode: {e}"))
                })
                .collect()
        })
        .map_err(PyRuntimeError::new_err)?;

    blobs
        .iter()
        .map(|b| Ok(PyBytes::new_bound(py, b)))
        .collect()
}


// ---------------------------------------------------------------------------
// render_many_par_cached
// ---------------------------------------------------------------------------

/// Like ``render_many_par`` but uses the process-global system font database.
/// Best choice for ProcessPoolExecutor + system fonts + large batches.
#[pyfunction]
#[pyo3(signature = (svg_strings, transform = None, bg_color = None, options = None))]
pub fn render_many_par_cached<'py>(
    py: Python<'py>,
    svg_strings: Vec<String>,
    transform: Option<(f64, f64, f64, f64, f64, f64)>,
    bg_color: Option<(u8, u8, u8, u8)>,
    options: Option<&RenderOptions>,
) -> PyResult<Vec<Bound<'py, PyBytes>>> {
    let fontdb = get_system_fontdb();
    let cloned_options = options.cloned();

    let blobs: Vec<Vec<u8>> = py
        .allow_threads(move || -> Result<Vec<Vec<u8>>, String> {
            let tr = transform_from_tuple(transform);
            let base_opts = build_usvg_options(cloned_options.as_ref(), fontdb);

            svg_strings
                .par_iter()
                .enumerate()
                .map(|(i, svg_str)| {
                    let tree = parse_svg_inner(svg_str, &base_opts)
                        .map_err(|e| format!("page {i}: {e}"))?;
                    let sz = tree.size().to_int_size();
                    let (w, h) = (sz.width(), sz.height());

                    let mut pixmap = make_pixmap(
                        match bg_color {
                            Some((r, g, b, a)) => Background::Colored(w, h, r, g, b, a),
                            None => Background::FromSvg,
                        },
                        sz,
                    )
                    .map_err(|e| format!("page {i}: {e}"))?;

                    resvg::render(&tree, tr, &mut pixmap.as_mut());
                    pixmap
                        .encode_png()
                        .map_err(|e| format!("page {i} PNG encode: {e}"))
                })
                .collect()
        })
        .map_err(PyRuntimeError::new_err)?;

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

    m.add_function(wrap_pyfunction!(svg_intrinsic_size, m)?)?;
    m.add_function(wrap_pyfunction!(svg_to_png, m)?)?;
    m.add_function(wrap_pyfunction!(svg_to_rgba, m)?)?;
    m.add_function(wrap_pyfunction!(render_many_par, m)?)?;

    m.add_function(wrap_pyfunction!(svg_to_png_cached, m)?)?;
    m.add_function(wrap_pyfunction!(svg_to_rgba_cached, m)?)?;
    m.add_function(wrap_pyfunction!(render_many_par_cached, m)?)?;

    Ok(())
}