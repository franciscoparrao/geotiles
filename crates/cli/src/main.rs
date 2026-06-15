//! geotiles — generate XYZ tile pyramids and MBTiles from geospatial rasters.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use indicatif::{ProgressBar, ProgressStyle};

use geotiles_core::{
    CogCompression, CogOptions, ColorScheme, MbtilesSink, MvtOptions, PyramidOptions,
    RasterSource, Resampling, SourceCrs, TileFormat, VectorSource, XyzSink, count_tiles, generate,
    generate_mvt, write_cog, write_cog_rgb,
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
    /// Tile a vector dataset (GeoJSON, GPKG) into MVT tiles.
    ///
    /// Output kind follows -o like `raster`: .mbtiles file or XYZ
    /// directory of .pbf tiles (uncompressed, static-server friendly).
    Vector(VectorArgs),
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
    /// Tile image format.
    #[arg(long, value_enum, default_value_t = FormatArg::Png)]
    format: FormatArg,
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

#[derive(clap::Args)]
struct VectorArgs {
    /// One or more input layers, each `[name=]path[#gpkg_table]`
    /// (GeoJSON or GeoPackage, EPSG:4326 or 3857). Each input becomes a
    /// layer in the tileset; the layer name defaults to the file stem.
    /// Examples: `cuencas.geojson`, `red=hidro.gpkg#rios`.
    #[arg(required = true, num_args = 1..)]
    inputs: Vec<String>,
    /// Output: file.mbtiles or a directory for the z/x/y.pbf tree.
    #[arg(short, long)]
    output: PathBuf,
    /// Tileset name in the output metadata (default: first layer name).
    #[arg(long)]
    name: Option<String>,
    /// Lowest zoom level.
    #[arg(long, default_value_t = 0)]
    min_zoom: u8,
    /// Highest zoom level.
    #[arg(long, default_value_t = 14)]
    max_zoom: u8,
    /// Simplification tolerance in tile units (0 disables).
    #[arg(long, default_value_t = 1.0)]
    simplify: f64,
    /// Override CRS detection.
    #[arg(long, value_enum)]
    source_crs: Option<CrsArg>,
}

/// One parsed `[name=]path[#gpkg_table]` layer spec.
struct LayerSpec {
    name: String,
    path: PathBuf,
    gpkg_table: Option<String>,
}

/// Parse `[name=]path[#gpkg_table]`. The layer name defaults to the file
/// stem; `#table` selects a GeoPackage table (lets one .gpkg supply
/// several layers).
fn parse_layer_spec(spec: &str) -> Result<LayerSpec> {
    let (name_opt, rest) = match spec.split_once('=') {
        Some((n, r)) => (Some(n.to_string()), r),
        None => (None, spec),
    };
    let (path_str, gpkg_table) = match rest.split_once('#') {
        Some((p, t)) => (p, Some(t.to_string())),
        None => (rest, None),
    };
    if path_str.is_empty() {
        anyhow::bail!("empty path in layer spec {spec:?}");
    }
    let path = PathBuf::from(path_str);
    let name = name_opt.unwrap_or_else(|| {
        path.file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "layer".into())
    });
    Ok(LayerSpec { name, path, gpkg_table })
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
enum FormatArg {
    /// PNG (lossless, universal).
    Png,
    /// Lossless WebP — smaller than PNG, no quality loss.
    Webp,
}

impl From<FormatArg> for TileFormat {
    fn from(v: FormatArg) -> Self {
        match v {
            FormatArg::Png => TileFormat::Png,
            FormatArg::Webp => TileFormat::WebP,
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
        format: args.format.into(),
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
        let mut sink = XyzSink::with_extension(&args.output, opts.format.as_str())?;
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

fn cmd_vector(args: VectorArgs) -> Result<()> {
    let crs = args.source_crs.map(Into::into);
    let specs: Vec<LayerSpec> = args
        .inputs
        .iter()
        .map(|s| parse_layer_spec(s))
        .collect::<Result<_>>()?;

    // First spec seeds the source; the rest stack on as extra layers.
    let (first, rest) = specs.split_first().expect("clap guarantees >= 1 input");
    let mut source = VectorSource::from_file(
        &first.path,
        &first.name,
        first.gpkg_table.as_deref(),
        crs,
    )
    .with_context(|| format!("reading {}", first.path.display()))?;
    for spec in rest {
        source
            .push_file(&spec.path, &spec.name, spec.gpkg_table.as_deref(), crs)
            .with_context(|| format!("reading {}", spec.path.display()))?;
    }

    let name = args.name.clone().unwrap_or_else(|| first.name.clone());

    let is_mbtiles = args
        .output
        .extension()
        .is_some_and(|e| e.eq_ignore_ascii_case("mbtiles"));
    let opts = MvtOptions {
        min_zoom: args.min_zoom,
        max_zoom: args.max_zoom,
        simplify: args.simplify,
        // Raw protobuf in XYZ trees so plain static servers work.
        compress: is_mbtiles,
        name,
        ..Default::default()
    };

    let n_features: usize = source.layers().iter().map(|l| l.features.len()).sum();
    eprintln!(
        "{} layer(s), {n_features} features, zooms {}..{}",
        source.layers().len(),
        opts.min_zoom,
        opts.max_zoom
    );

    let stats = if is_mbtiles {
        let mut sink = MbtilesSink::create(&args.output)?;
        generate_mvt(&source, &opts, &mut sink, |_| {})?
    } else {
        let mut sink = XyzSink::with_extension(&args.output, "pbf")?;
        generate_mvt(&source, &opts, &mut sink, |_| {})?
    };

    println!(
        "MVT: {} tiles written, {} empty tiles skipped → {}",
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
        Command::Vector(args) => cmd_vector(args),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layer_spec_plain_path_uses_stem() {
        let s = parse_layer_spec("data/cuencas.geojson").unwrap();
        assert_eq!(s.name, "cuencas");
        assert_eq!(s.path, PathBuf::from("data/cuencas.geojson"));
        assert!(s.gpkg_table.is_none());
    }

    #[test]
    fn layer_spec_with_name_and_table() {
        let s = parse_layer_spec("red=hidro.gpkg#rios").unwrap();
        assert_eq!(s.name, "red");
        assert_eq!(s.path, PathBuf::from("hidro.gpkg"));
        assert_eq!(s.gpkg_table.as_deref(), Some("rios"));
    }

    #[test]
    fn layer_spec_table_without_name() {
        let s = parse_layer_spec("hidro.gpkg#rios").unwrap();
        assert_eq!(s.name, "hidro");
        assert_eq!(s.gpkg_table.as_deref(), Some("rios"));
    }

    #[test]
    fn layer_spec_empty_path_rejected() {
        assert!(parse_layer_spec("red=").is_err());
    }

    #[test]
    fn verify_cli() {
        use clap::CommandFactory;
        Cli::command().debug_assert();
    }
}
