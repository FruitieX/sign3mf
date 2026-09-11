//! Bitmap to multipart printable sign conversion.
//!
//! The public pipeline deliberately keeps image preparation, tracing, mesh
//! generation, and packaging independent.  The default tracer is a shared
//! boundary pixel tracer.  A VTracer adapter can be enabled by downstream
//! applications with the `vtracer-backend` feature without changing the
//! printable geometry API.

pub mod color;
pub mod geometry;
pub mod mesh;
pub mod package;
pub mod preprocess;
pub mod svg;
pub mod trace;

use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};

pub use color::Rgb;
pub use geometry::{Contour, Point};
pub use mesh::{Mesh, Part, Triangle, Vertex};
pub use preprocess::{Filter, PreparedImage};
pub use trace::{Region, TraceMode};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    FullDepth,
    Backed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartsMode {
    Color,
    Region,
}

#[derive(Debug, Clone)]
pub struct Config {
    pub input: PathBuf,
    pub output: PathBuf,
    pub width: Option<f64>,
    pub height: Option<f64>,
    pub thickness: f64,
    pub colors: usize,
    pub palette: Option<Vec<Rgb>>,
    pub mode: Mode,
    pub upside_down: bool,
    pub face_depth: f64,
    pub backing_color: Option<Rgb>,
    pub upscale: u32,
    pub filter: Filter,
    pub trace_mode: TraceMode,
    pub simplify: f64,
    pub curve_tolerance: f64,
    pub speckle_area: Option<f64>,
    pub remove_background: Option<Rgb>,
    pub background_tolerance: u8,
    pub alpha_threshold: u8,
    pub trim: bool,
    pub parts: PartsMode,
    pub svg_out: Option<PathBuf>,
    pub preview_out: Option<PathBuf>,
    pub force: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            input: PathBuf::new(),
            output: PathBuf::new(),
            width: None,
            height: None,
            thickness: 3.0,
            colors: 2,
            palette: None,
            mode: Mode::Backed,
            upside_down: true,
            face_depth: 1.0,
            backing_color: None,
            upscale: 4,
            filter: Filter::Cubic,
            trace_mode: TraceMode::Spline,
            simplify: 0.0,
            curve_tolerance: 0.05,
            speckle_area: None,
            remove_background: None,
            background_tolerance: 20,
            alpha_threshold: 128,
            trim: false,
            parts: PartsMode::Color,
            svg_out: None,
            preview_out: None,
            force: false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ConversionReport {
    pub width: f64,
    pub height: f64,
    pub palette: Vec<Rgb>,
    pub parts: usize,
    pub triangles: usize,
    pub disconnected_pieces: usize,
}

#[derive(Debug, Clone)]
pub struct ConversionResult {
    pub parts: Vec<Part>,
    pub report: ConversionReport,
    pub prepared: PreparedImage,
}

pub fn convert(config: &Config) -> Result<ConversionResult> {
    validate_config(config)?;
    let mut prepared = preprocess::load_and_prepare(config).context("preparing input artwork")?;
    let traced = trace::trace(&mut prepared, config).context("tracing artwork boundaries")?;
    let parts = mesh::build_parts(&traced, config).context("building printable meshes")?;
    if parts.is_empty() {
        return Err(anyhow!(
            "artwork contains no printable regions after preprocessing"
        ));
    }
    mesh::validate_parts(&parts).context("validating printable geometry")?;
    let (width, height) = mesh::bounds(&parts);
    let triangles = parts.iter().map(|p| p.mesh.triangles.len()).sum();
    let report = ConversionReport {
        width,
        height,
        palette: traced.palette.clone(),
        parts: parts.len(),
        triangles,
        disconnected_pieces: traced.disconnected_pieces,
    };
    Ok(ConversionResult {
        parts,
        report,
        prepared,
    })
}

pub fn convert_and_write(config: &Config) -> Result<ConversionReport> {
    if config.output.exists() && !config.force {
        return Err(anyhow!(
            "output {} already exists; pass --force to replace it",
            config.output.display()
        ));
    }
    let result = convert(config)?;
    let tmp_name = format!(
        ".{}.sign3d.tmp",
        config
            .output
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("output")
    );
    let tmp_path = config.output.with_file_name(tmp_name);
    package::write_3mf(&tmp_path, &result.parts).context("writing temporary 3MF output")?;
    if config.output.exists() && config.force {
        std::fs::remove_file(&config.output).context("replacing existing 3MF output")?;
    }
    std::fs::rename(&tmp_path, &config.output).context("committing 3MF output")?;
    if let Some(path) = &config.svg_out {
        svg::write_svg(path, &result.parts).context("writing SVG output")?;
    }
    if let Some(path) = &config.preview_out {
        preprocess::write_preview(path, &result.prepared, &result.parts)
            .context("writing preview")?;
    }
    Ok(result.report)
}

fn validate_config(c: &Config) -> Result<()> {
    if c.input.as_os_str().is_empty() {
        return Err(anyhow!("--input is required"));
    }
    if c.output.as_os_str().is_empty() {
        return Err(anyhow!("--output is required"));
    }
    if c.width.is_some() == c.height.is_some() {
        return Err(anyhow!("provide exactly one of --width or --height"));
    }
    let requested = c.width.or(c.height).unwrap();
    if !requested.is_finite() || requested <= 0.0 {
        return Err(anyhow!("requested width/height must be positive"));
    }
    if !c.thickness.is_finite() || c.thickness <= 0.0 {
        return Err(anyhow!("--thickness must be positive"));
    }
    if c.colors == 0 {
        return Err(anyhow!("--colors must be at least 1"));
    }
    if let Some(p) = &c.palette {
        if p.is_empty() {
            return Err(anyhow!("--palette cannot be empty"));
        }
    }
    if c.mode == Mode::Backed
        && (!c.face_depth.is_finite() || c.face_depth <= 0.0 || c.face_depth >= c.thickness)
    {
        return Err(anyhow!(
            "--face-depth must be greater than zero and smaller than --thickness in backed mode"
        ));
    }
    if c.mode == Mode::FullDepth && c.backing_color.is_some() {
        return Err(anyhow!("--backing-color is only valid with --mode backed"));
    }
    if c.upscale == 0 {
        return Err(anyhow!("--upscale must be at least 1"));
    }
    if c.simplify < 0.0 || c.curve_tolerance <= 0.0 {
        return Err(anyhow!("geometry tolerances must be non-negative"));
    }
    if !c.input.exists() {
        return Err(anyhow!("input {} does not exist", c.input.display()));
    }
    Ok(())
}

pub fn ensure_parent(path: &Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    Ok(())
}
