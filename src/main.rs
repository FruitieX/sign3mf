use anyhow::{anyhow, Result};
use clap::{ArgAction, Parser, ValueEnum};
use sign3mf::{
    color::parse_hex, color::parse_palette, convert_and_write, Config, CountersinkSide, Filter,
    HolePosition, HoleStyle, Mode, PartsMode, TraceMode,
};
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(
    name = "sign3mf",
    version,
    about = "Convert bitmap artwork into a multipart printable 3MF sign"
)]
struct Args {
    #[arg(long)]
    input: PathBuf,
    #[arg(long)]
    output: PathBuf,
    #[arg(
        long,
        conflicts_with = "height",
        help = "Final sign width in millimetres (mm). Exactly one of --width or --height is required."
    )]
    width: Option<f64>,
    #[arg(
        long,
        conflicts_with = "width",
        help = "Final sign height in millimetres (mm). Exactly one of --width or --height is required."
    )]
    height: Option<f64>,
    #[arg(
        long,
        default_value_t = 3.0,
        help = "Total sign thickness in millimetres (mm)."
    )]
    thickness: f64,
    #[arg(long, help = "Maximum number of artwork colors. Default: 2.")]
    colors: Option<usize>,
    #[arg(long)]
    palette: Option<String>,
    #[arg(long, value_enum, default_value_t=ModeArg::Backed, help = "Geometry mode: backed uses a backing layer plus a colored face; full-depth extrudes each color through the full thickness.")]
    mode: ModeArg,
    #[arg(long, value_enum, default_value_t = OrientationArg::UpsideDown, help = "Print orientation: upside-down rotates 180° around Y, putting the colored face against the build plate. Default: upside-down.")]
    orientation: OrientationArg,
    #[arg(
        long,
        default_value_t = 1.0,
        help = "Colored face depth in millimetres (mm) for backed mode."
    )]
    face_depth: f64,
    #[arg(
        long,
        default_value_t = 0.0,
        help = "Round the sign outline corners with this fillet radius in millimetres (mm)."
    )]
    corner_radius: f64,
    #[arg(
        long,
        default_value_t = 0.0,
        help = "Chamfer exposed exterior edges and corners by this distance in millimetres (mm); must be less than half the total sign thickness. In backed mode the backing/face interface stays flat."
    )]
    chamfer: f64,
    #[arg(
        long,
        value_enum,
        default_value_t = HoleStyleArg::Round,
        help = "Screw-hole shape: round is a cylindrical through-hole; countersunk adds a tapered seat on the selected side."
    )]
    hole_style: HoleStyleArg,
    #[arg(
        long,
        help = "Countersink opening diameter in millimetres (mm); defaults to the bore plus the configured countersink depth and angle."
    )]
    countersink_diameter: Option<f64>,
    #[arg(
        long,
        default_value_t = 90.0,
        help = "Included countersink angle in degrees."
    )]
    countersink_angle: f64,
    #[arg(
        long,
        default_value_t = 1.0,
        help = "Countersink depth in millimetres (mm) when --countersink-diameter is omitted."
    )]
    countersink_depth: f64,
    #[arg(
        long,
        value_enum,
        default_value_t = CountersinkSideArg::Front,
        help = "Countersink side: front is the artwork face (default); back is the backing face."
    )]
    countersink_side: CountersinkSideArg,
    #[arg(
        long,
        default_value_t = 5.0,
        help = "Round screw-hole diameter in millimetres (mm). Used with --hole."
    )]
    hole_diameter: f64,
    #[arg(
        long,
        default_value_t = 5.0,
        help = "Distance from the nearest sign edge to each hole centre in millimetres (mm)."
    )]
    hole_inset: f64,
    #[arg(
        long = "hole",
        value_enum,
        value_delimiter = ',',
        action = ArgAction::Append,
        help = "Add round through-holes at corners, center-top, center-left, center-right, or center-bottom. Repeat or comma-separate."
    )]
    holes: Vec<HoleArg>,
    #[arg(long)]
    backing_color: Option<String>,
    #[arg(
        long,
        default_value_t = 4,
        help = "Bitmap upscaling factor before tracing. Default: 4."
    )]
    upscale: u32,
    #[arg(long, value_enum, default_value_t=FilterArg::Cubic)]
    filter: FilterArg,
    #[arg(long, value_enum, default_value_t=TraceArg::Spline)]
    trace_mode: TraceArg,
    #[arg(
        long,
        default_value_t = 0.0,
        help = "Shared-boundary simplification tolerance in millimetres (mm)."
    )]
    simplify: f64,
    #[arg(
        long,
        default_value_t = 0.05,
        help = "Curve flattening tolerance in millimetres (mm)."
    )]
    curve_tolerance: f64,
    #[arg(
        long,
        help = "Remove color regions smaller than this area in square millimetres (mm²)."
    )]
    speckle_area: Option<f64>,
    #[arg(long)]
    remove_background: Option<String>,
    #[arg(long, default_value_t = 20)]
    background_tolerance: u8,
    #[arg(long, default_value_t = 128)]
    alpha_threshold: u8,
    #[arg(long)]
    trim: bool,
    #[arg(long, value_enum, default_value_t=PartsArg::Color)]
    parts: PartsArg,
    #[arg(long)]
    svg_out: Option<PathBuf>,
    #[arg(long)]
    preview_out: Option<PathBuf>,
    #[arg(long)]
    force: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
enum ModeArg {
    FullDepth,
    Backed,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
enum OrientationArg {
    UpsideDown,
    RightSideUp,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
enum HoleArg {
    Corners,
    CenterTop,
    CenterLeft,
    CenterRight,
    CenterBottom,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
enum HoleStyleArg {
    Round,
    Countersunk,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
enum CountersinkSideArg {
    Front,
    Back,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
enum FilterArg {
    Nearest,
    Cubic,
    Lanczos,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
enum TraceArg {
    Pixel,
    Polygon,
    Spline,
}
#[derive(Debug, Clone, Copy, ValueEnum)]
enum PartsArg {
    Color,
    Region,
}

fn main() -> Result<()> {
    let args = Args::parse();
    if args.width.is_none() && args.height.is_none() {
        return Err(anyhow!("provide exactly one of --width or --height"));
    }
    if args.colors.is_some() && args.palette.is_some() {
        return Err(anyhow!("--palette conflicts with --colors"));
    }
    let config = Config {
        input: args.input,
        output: args.output,
        width: args.width,
        height: args.height,
        thickness: args.thickness,
        colors: args.colors.unwrap_or(2),
        palette: args.palette.as_deref().map(parse_palette).transpose()?,
        mode: match args.mode {
            ModeArg::FullDepth => Mode::FullDepth,
            ModeArg::Backed => Mode::Backed,
        },
        upside_down: matches!(args.orientation, OrientationArg::UpsideDown),
        face_depth: args.face_depth,
        corner_radius: args.corner_radius,
        chamfer: args.chamfer,
        hole_style: match args.hole_style {
            HoleStyleArg::Round => HoleStyle::Round,
            HoleStyleArg::Countersunk => HoleStyle::Countersunk,
        },
        countersink_diameter: args.countersink_diameter,
        countersink_angle: args.countersink_angle,
        countersink_depth: args.countersink_depth,
        countersink_side: match args.countersink_side {
            CountersinkSideArg::Front => CountersinkSide::Front,
            CountersinkSideArg::Back => CountersinkSide::Back,
        },
        hole_diameter: args.hole_diameter,
        hole_inset: args.hole_inset,
        holes: args
            .holes
            .into_iter()
            .map(|hole| match hole {
                HoleArg::Corners => HolePosition::Corners,
                HoleArg::CenterTop => HolePosition::CenterTop,
                HoleArg::CenterLeft => HolePosition::CenterLeft,
                HoleArg::CenterRight => HolePosition::CenterRight,
                HoleArg::CenterBottom => HolePosition::CenterBottom,
            })
            .collect(),
        backing_color: args.backing_color.as_deref().map(parse_hex).transpose()?,
        upscale: args.upscale,
        filter: match args.filter {
            FilterArg::Nearest => Filter::Nearest,
            FilterArg::Cubic => Filter::Cubic,
            FilterArg::Lanczos => Filter::Lanczos,
        },
        trace_mode: match args.trace_mode {
            TraceArg::Pixel => TraceMode::Pixel,
            TraceArg::Polygon => TraceMode::Polygon,
            TraceArg::Spline => TraceMode::Spline,
        },
        simplify: args.simplify,
        curve_tolerance: args.curve_tolerance,
        speckle_area: args.speckle_area,
        remove_background: args
            .remove_background
            .as_deref()
            .map(parse_hex)
            .transpose()?,
        background_tolerance: args.background_tolerance,
        alpha_threshold: args.alpha_threshold,
        trim: args.trim,
        parts: match args.parts {
            PartsArg::Color => PartsMode::Color,
            PartsArg::Region => PartsMode::Region,
        },
        svg_out: args.svg_out,
        preview_out: args.preview_out,
        force: args.force,
    };
    let report = convert_and_write(&config)?;
    println!(
        "wrote {:.2} × {:.2} mm; {} colors, {} parts, {} triangles; {} disconnected piece(s)",
        report.width,
        report.height,
        report.palette.len(),
        report.parts,
        report.triangles,
        report.disconnected_pieces
    );
    println!(
        "palette: {}",
        report
            .palette
            .iter()
            .map(|c| c.to_string())
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(())
}
