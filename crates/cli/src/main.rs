//! geotiles — generate XYZ tile pyramids and MBTiles from geospatial rasters.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};

use geotiles_core::{
    CogCompression, CogOptions, ColorScheme, MbtilesSink, PyramidOptions, RasterSource,
    Resampling, SourceCrs, XyzSink, count_tiles, generate, write_cog, write_cog_rgb,
};

#[derive(Parser)]
#[command(name = "geotiles", version, about = "Web map tiles from geospatial rasters")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Tile a raster into an MBTiles file or an XYZ directory tree.
    ///
    /// The output kind is inferred from -o: a path ending in .mbtiles
    /// produces an MBTiles file, anything else an XYZ directory.
    Raster(RasterArgs),
    /// Rewrite a raster as a Cloud Optimized GeoTIFF (Float32 + overviews).
    Cog {
        /// Input raster (GeoTIFF) in EPSG:4326 or EPSG:3857.
        input: PathBuf,
        /// Output COG path.
        #[arg(short, long)]
        output: PathBuf,
        /// Internal tile size in pixels (multiple of 16).
        #[arg(long, default_value_t = 512)]
        tile_size: u32,
        /// Disable deflate compression.
        #[arg(long)]
        no_compression: bool,
        /// Override CRS detection.
        #[arg(long, value_enum)]
        source_crs: Option<CrsArg>,
        /// Bands to convert, 1-based: one band (Float32 COG) or 3-4
        /// (byte RGB/RGBA COG), e.g. --bands 1,2,3.
        #[arg(long, default_value = "1", value_delimiter = ',')]
        bands: Vec<usize>,
        /// Stretch for RGB byte output as MIN,MAX (default 0,255).
        #[arg(long, value_parser = parse_range)]
        range: Option<(f64, f64)>,
    },
    /// Show tiling-relevant information about a raster.
    Info {
        /// Input raster (GeoTIFF).
        input: PathBuf,
    },
}

#[derive(clap::Args)]
struct RasterArgs {
    /// Input raster (GeoTIFF) in EPSG:4326 or EPSG:3857.
    input: PathBuf,
    /// Output: file.mbtiles or a directory for the z/x/y.png tree.
    #[arg(short, long)]
    output: PathBuf,
    /// Lowest zoom level (default 0).
    #[arg(long)]
    min_zoom: Option<u8>,
    /// Highest zoom level (default: native resolution of the input).
    #[arg(long)]
    max_zoom: Option<u8>,
    /// Colour scheme. Use --list-schemes to see all options.
    #[arg(long, default_value = "grayscale")]
    scheme: String,
    /// List available colour schemes and exit.
    #[arg(long)]
    list_schemes: bool,
    /// Resampling method.
    #[arg(long, value_enum, default_value_t = ResampleArg::Bilinear)]
    resample: ResampleArg,
    /// Fixed stretch as MIN,MAX (default: data min/max).
    #[arg(long, value_parser = parse_range)]
    range: Option<(f64, f64)>,
    /// Layer name written to the output metadata (default: input stem).
    #[arg(long)]
    name: Option<String>,
    /// Override CRS detection.
    #[arg(long, value_enum)]
    source_crs: Option<CrsArg>,
    /// Bands to tile, 1-based: one band (colour scheme applied) or 3-4
    /// bands rendered as RGB(A), e.g. --bands 4,3,2.
    #[arg(long, default_value = "1", value_delimiter = ',')]
    bands: Vec<usize>,
}

#[derive(Clone, Copy, ValueEnum)]
enum ResampleArg {
    Nearest,
    Bilinear,
}

impl From<ResampleArg> for Resampling {
    fn from(v: ResampleArg) -> Self {
        match v {
            ResampleArg::Nearest => Resampling::Nearest,
            ResampleArg::Bilinear => Resampling::Bilinear,
        }
    }
}

#[derive(Clone, Copy, ValueEnum)]
enum CrsArg {
    /// EPSG:4326 lon/lat degrees.
    #[value(name = "4326", alias = "lonlat", alias = "wgs84")]
    LonLat,
    /// EPSG:3857 Web Mercator meters.
    #[value(name = "3857", alias = "mercator")]
    Mercator,
}

impl From<CrsArg> for SourceCrs {
    fn from(v: CrsArg) -> Self {
        match v {
            CrsArg::LonLat => SourceCrs::LonLat,
            CrsArg::Mercator => SourceCrs::Mercator,
        }
    }
}

fn parse_range(s: &str) -> Result<(f64, f64), String> {
    let (lo, hi) = s
        .split_once(',')
        .ok_or_else(|| "expected MIN,MAX (e.g. 0,2500)".to_string())?;
    let lo: f64 = lo.trim().parse().map_err(|e| format!("bad MIN: {e}"))?;
    let hi: f64 = hi.trim().parse().map_err(|e| format!("bad MAX: {e}"))?;
    if lo >= hi {
        return Err(format!("MIN ({lo}) must be < MAX ({hi})"));
    }
    Ok((lo, hi))
}

fn parse_scheme(name: &str) -> Result<ColorScheme> {
    let wanted = name.to_lowercase();
    ColorScheme::ALL
        .iter()
        .copied()
        .find(|s| s.name().to_lowercase() == wanted)
        .with_context(|| {
            format!("unknown scheme {name:?}; available: {}", scheme_names().join(", "))
        })
}

fn scheme_names() -> Vec<String> {
    ColorScheme::ALL.iter().map(|s| s.name().to_lowercase()).collect()
}

fn validate_band_selection(bands: &[usize]) -> Result<()> {
    if !matches!(bands.len(), 1 | 3 | 4) {
        anyhow::bail!("--bands takes 1 (gray), 3 (RGB) or 4 (RGBA) bands, got {}", bands.len());
    }
    Ok(())
}

fn load_source(input: &Path, bands: &[usize], crs: Option<SourceCrs>) -> Result<RasterSource> {
    validate_band_selection(bands)?;
    let rasters = geotiles_core::read_bands(input, Some(bands))
        .with_context(|| format!("reading {}", input.display()))?;
    RasterSource::new_multi(rasters, crs).context("preparing raster source")
}

fn cmd_raster(args: RasterArgs) -> Result<()> {
    if args.list_schemes {
        println!("{}", scheme_names().join("\n"));
        return Ok(());
    }

    let source = load_source(&args.input, &args.bands, args.source_crs.map(Into::into))?;
    let name = args.name.clone().unwrap_or_else(|| {
        args.input
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "geotiles".into())
    });
    let opts = PyramidOptions {
        min_zoom: args.min_zoom,
        max_zoom: args.max_zoom,
        resampling: args.resample.into(),
        scheme: parse_scheme(&args.scheme)?,
        range: args.range,
        name,
        ..Default::default()
    };

    let total = count_tiles(&source, &opts)?;
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} [{bar:40.cyan/blue}] {pos}/{len} tiles ({eta})",
        )?
        .progress_chars("=> "),
    );

    let is_mbtiles = args
        .output
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("mbtiles"));

    let stats = if is_mbtiles {
        let mut sink = MbtilesSink::create(&args.output)?;
        generate(&source, &opts, &mut sink, |done| pb.set_position(done))?
    } else {
        let mut sink = XyzSink::create(&args.output)?;
        generate(&source, &opts, &mut sink, |done| pb.set_position(done))?
    };
    pb.finish_and_clear();

    println!(
        "{}: {} tiles written, {} empty tiles skipped → {}",
        if is_mbtiles { "MBTiles" } else { "XYZ" },
        stats.written,
        stats.skipped,
        args.output.display()
    );
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn cmd_cog(
    input: &Path,
    output: &Path,
    tile_size: u32,
    no_compression: bool,
    source_crs: Option<SourceCrs>,
    bands: &[usize],
    range: Option<(f64, f64)>,
) -> Result<()> {
    validate_band_selection(bands)?;
    let rasters = geotiles_core::read_bands(input, Some(bands))
        .with_context(|| format!("reading {}", input.display()))?;
    let opts = CogOptions {
        tile_size,
        compression: if no_compression { CogCompression::None } else { CogCompression::Deflate },
        crs: source_crs,
    };
    let info = if rasters.len() == 1 {
        write_cog(&rasters[0], output, &opts)?
    } else {
        write_cog_rgb(&rasters, output, &opts, range)?
    };
    println!(
        "COG ({}): {} levels, {} tiles, {:.1} MiB → {}",
        if rasters.len() == 1 { "Float32" } else { "byte RGB(A)" },
        info.levels,
        info.tiles,
        info.file_size as f64 / (1024.0 * 1024.0),
        output.display()
    );
    Ok(())
}

fn cmd_info(input: &Path) -> Result<()> {
    let n_bands = geotiles_core::band_count(input)
        .with_context(|| format!("reading {}", input.display()))?;
    let source = load_source(input, &[1], None)?;
    let raster = source.raster();
    let (w, s, e, n) = source.bounds_lonlat();

    println!("input        : {}", input.display());
    println!("size         : {} cols × {} rows × {} band(s)", raster.cols(), raster.rows(), n_bands);
    println!(
        "crs          : {}",
        match source.crs() {
            SourceCrs::LonLat => "EPSG:4326 (lon/lat)",
            SourceCrs::Mercator => "EPSG:3857 (Web Mercator)",
        }
    );
    println!("bounds (ll)  : {w:.6}, {s:.6} → {e:.6}, {n:.6}");
    println!("cell size    : {} (source units)", raster.cell_size());
    println!("resolution   : {:.2} m/px (mercator approx.)", source.native_resolution_m());
    println!("native zoom  : {}", source.native_max_zoom(256));
    if let Some(nd) = raster.nodata() {
        println!("nodata       : {nd}");
    }
    Ok(())
}

fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Raster(args) => cmd_raster(args),
        Command::Cog { input, output, tile_size, no_compression, source_crs, bands, range } => {
            cmd_cog(
                &input,
                &output,
                tile_size,
                no_compression,
                source_crs.map(Into::into),
                &bands,
                range,
            )
        }
        Command::Info { input } => cmd_info(&input),
    }
}
