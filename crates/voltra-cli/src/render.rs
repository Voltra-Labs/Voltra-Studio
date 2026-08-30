//! The `render` subcommand: a scene, composited and written out as Y4M.
//!
//! This is the first thing Voltra produces that a person can watch. Y4M is raw
//! video with a text header, so it needs no encoder and no container, and it
//! pipes straight into anything:
//!
//! ```text
//! voltra render -o - --frames 300 | ffplay -
//! voltra render -o - --frames 300 | x264 --demuxer y4m - -o demo.mp4
//! ```
//!
//! The loop follows the order libobs uses and plan 008 adopted: tick, place,
//! composite, convert, write. Each of the last three is timed separately, so
//! the summary says where the frame time actually went instead of averaging the
//! three into one useless number.

use std::io::Write;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use voltra_core::{
    ColorRange, ColorSpec, Fps, FrameSize, PixelFormat, ScaleFilter, TickContext, Timestamp,
    VideoFrame, rgb_to_yuv,
};
use voltra_output::{Y4mParams, Y4mWriter};
use voltra_render::{Compositor, CpuCompositor};

use crate::demo::DemoScene;

/// Arguments of `voltra render`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// Where to write the Y4M stream. `-` is standard output.
    #[arg(short, long, value_name = "PATH")]
    output: PathBuf,

    /// Canvas size, as `WIDTHxHEIGHT`.
    #[arg(long, default_value = "1280x720", value_parser = parse_size)]
    size: FrameSize,

    /// Frame rate, as `60` or as an exact ratio like `30000/1001`.
    #[arg(long, default_value = "60", value_parser = parse_fps)]
    fps: Fps,

    /// How many frames to render.
    #[arg(short = 'n', long, default_value_t = 120)]
    frames: u64,

    /// Canvas clear colour, as `RRGGBB`.
    #[arg(long, default_value = "101820", value_parser = parse_color)]
    background: [u8; 4],

    /// How scaled items are resampled.
    #[arg(long, default_value = "bilinear", value_parser = parse_filter)]
    filter: ScaleFilter,

    /// Convert to full range instead of the broadcast 16–235.
    #[arg(long)]
    full_range: bool,
}

/// Render the demonstration scene and write it out.
///
/// # Errors
///
/// When the destination cannot be opened or written, or when compositing or
/// converting fails.
pub fn run(args: &Args) -> Result<()> {
    let spec = ColorSpec::new(
        voltra_core::ColorSpace::Bt709,
        if args.full_range {
            ColorRange::Full
        } else {
            ColorRange::Limited
        },
    );
    if args.size.height() < 720 {
        // Y4M has no field for primaries or matrix, so players fall back to a
        // convention: BT.601 below 720 lines, BT.709 from there up. See
        // docs/references/y4m.md §2.
        tracing::warn!(
            height = args.size.height(),
            "converting with BT.709, but players assume BT.601 below 720 lines"
        );
    }

    let sink = open_sink(&args.output)?;
    let params = Y4mParams {
        fps: args.fps,
        range: spec.range,
        ..Y4mParams::default()
    };
    let mut writer = Y4mWriter::new(sink, PixelFormat::I420, args.size, params)
        .context("writing the Y4M header")?;

    let mut demo = DemoScene::new(args.size, args.filter).context("building the demo scene")?;
    let mut compositor = CpuCompositor::new(args.size).context("building the compositor")?;
    compositor.set_background(args.background);

    // Allocated once and rewritten every frame. Allocating a 1080p I420 frame
    // costs 133 µs (docs/PERFORMANCE.md §2); doing it per frame would be 0.8 %
    // of the budget spent on nothing.
    let mut encoded = VideoFrame::new(PixelFormat::I420, args.size, Timestamp::ZERO)
        .context("allocating the conversion target")?;

    let mut report = Report::default();
    let mut context = TickContext::new(0, args.fps);
    let started = Instant::now();

    for index in 0..args.frames {
        let pts = args.fps.pts(index);
        demo.tick(&context).context("ticking the scene")?;

        let mark = Instant::now();
        let canvas = compositor
            .composite(demo.scene(), &demo, pts)
            .context("compositing")?;
        report.composite.add(mark.elapsed());

        let mark = Instant::now();
        encoded.set_pts(pts);
        rgb_to_yuv(canvas, &mut encoded, spec).context("converting to I420")?;
        report.convert.add(mark.elapsed());

        let mark = Instant::now();
        writer.write_frame(&encoded).context("writing a frame")?;
        report.write.add(mark.elapsed());

        context = context.next();
    }

    let wall = started.elapsed();
    writer.finish().context("flushing the stream")?;
    report.print(&mut std::io::stderr(), args, wall)?;
    Ok(())
}

/// Open the destination, or standard output for `-`.
fn open_sink(path: &std::path::Path) -> Result<Box<dyn Write>> {
    if path.as_os_str() == "-" {
        // Locked once here rather than per write. `StdoutLock` is line
        // buffered, which is the wrong shape for megabyte writes, but the plane
        // writes are far larger than the buffer so they pass straight through.
        return Ok(Box::new(std::io::stdout().lock()));
    }
    let file =
        std::fs::File::create(path).with_context(|| format!("creating {}", path.display()))?;
    // Big enough that the header and the frame markers do not become their own
    // syscalls; plane writes exceed it and go straight to the file.
    Ok(Box::new(std::io::BufWriter::with_capacity(64 * 1024, file)))
}

/// Running minimum, maximum and total of one stage's cost.
#[derive(Debug, Default, Clone, Copy)]
struct Stage {
    total: Duration,
    max: Duration,
    count: u32,
}

impl Stage {
    fn add(&mut self, elapsed: Duration) {
        self.total += elapsed;
        self.max = self.max.max(elapsed);
        self.count += 1;
    }

    fn average(self) -> Duration {
        self.total.checked_div(self.count).unwrap_or_default()
    }
}

/// What the run cost, per stage.
///
/// Always on, per CLAUDE.md §4.10: a pipeline whose cost nobody watches is one
/// that quietly degrades.
#[derive(Debug, Default)]
struct Report {
    composite: Stage,
    convert: Stage,
    write: Stage,
}

impl Report {
    /// Write the summary. Goes to stderr so `-o -` stays a clean video pipe.
    fn print(&self, out: &mut impl Write, args: &Args, wall: Duration) -> std::io::Result<()> {
        let budget = args.fps.frame_duration();
        let per_frame = self.composite.average() + self.convert.average() + self.write.average();

        writeln!(
            out,
            "rendered {} frames of {} at {:.3} fps in {:.3} s",
            args.frames,
            args.size,
            args.fps.as_f64(),
            wall.as_secs_f64()
        )?;
        for (name, stage) in [
            ("composite", self.composite),
            ("convert", self.convert),
            ("write", self.write),
        ] {
            writeln!(
                out,
                "  {name:<10} avg {:>8.3} ms   max {:>8.3} ms",
                millis(stage.average()),
                millis(stage.max)
            )?;
        }
        writeln!(
            out,
            "  {:<10} avg {:>8.3} ms   budget {:.3} ms ({:.2}x)",
            "total",
            millis(per_frame),
            millis(budget),
            millis(per_frame) / millis(budget).max(f64::EPSILON)
        )?;
        Ok(())
    }
}

fn millis(duration: Duration) -> f64 {
    duration.as_secs_f64() * 1_000.0
}

/// Parse `WIDTHxHEIGHT`.
fn parse_size(value: &str) -> std::result::Result<FrameSize, String> {
    let (width, height) = value
        .split_once(['x', 'X'])
        .ok_or_else(|| format!("expected WIDTHxHEIGHT, got `{value}`"))?;
    let width: u32 = width
        .trim()
        .parse()
        .map_err(|_| format!("bad width `{width}`"))?;
    let height: u32 = height
        .trim()
        .parse()
        .map_err(|_| format!("bad height `{height}`"))?;
    if width % 2 != 0 || height % 2 != 0 {
        return Err(format!("{width}x{height}: 4:2:0 needs even dimensions"));
    }
    FrameSize::new(width, height).map_err(|error| error.to_string())
}

/// Parse `60`, `30000/1001` or `30000:1001`.
///
/// Kept as an exact ratio rather than a float, which is the whole reason the
/// clock of plan 002 is rational: 29.97 written as a decimal drifts.
fn parse_fps(value: &str) -> std::result::Result<Fps, String> {
    let (num, den) = match value.split_once(['/', ':']) {
        Some((num, den)) => (num, den),
        None => (value, "1"),
    };
    let num: u32 = num
        .trim()
        .parse()
        .map_err(|_| format!("bad frame rate `{value}`"))?;
    let den: u32 = den
        .trim()
        .parse()
        .map_err(|_| format!("bad frame rate `{value}`"))?;
    Fps::new(num, den).map_err(|error| error.to_string())
}

/// Parse `RRGGBB` into an opaque `[r, g, b, a]`.
fn parse_color(value: &str) -> std::result::Result<[u8; 4], String> {
    let hex = value.strip_prefix('#').unwrap_or(value);
    if hex.len() != 6 {
        return Err(format!("expected RRGGBB, got `{value}`"));
    }
    let mut color = [0, 0, 0, 255];
    for (index, channel) in color.iter_mut().take(3).enumerate() {
        let start = index * 2;
        *channel = u8::from_str_radix(&hex[start..start + 2], 16)
            .map_err(|_| format!("`{value}` is not hexadecimal"))?;
    }
    Ok(color)
}

/// Parse a scale filter by name.
fn parse_filter(value: &str) -> std::result::Result<ScaleFilter, String> {
    match value.to_ascii_lowercase().as_str() {
        "point" | "nearest" => Ok(ScaleFilter::Point),
        "bilinear" => Ok(ScaleFilter::Bilinear),
        "bicubic" => Ok(ScaleFilter::Bicubic),
        "lanczos" => Ok(ScaleFilter::Lanczos),
        "area" => Ok(ScaleFilter::Area),
        other => Err(format!("unknown scale filter `{other}`")),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_color, parse_filter, parse_fps, parse_size};
    use voltra_core::ScaleFilter;

    #[test]
    fn sizes_parse_and_reject_odd_dimensions() {
        let size = parse_size("1920x1080").unwrap();
        assert_eq!((size.width(), size.height()), (1920, 1080));
        assert!(parse_size("1921x1080").is_err());
        assert!(parse_size("1920").is_err());
        assert!(parse_size("axb").is_err());
    }

    /// The reason the clock is rational, checked at the command line too.
    #[test]
    fn frame_rates_keep_their_exact_ratio() {
        assert_eq!(parse_fps("60").unwrap(), voltra_core::Fps::FPS_60);
        let ntsc = parse_fps("30000/1001").unwrap();
        assert_eq!((ntsc.num(), ntsc.den()), (30000, 1001));
        assert_eq!(parse_fps("30000:1001").unwrap(), ntsc);
        assert!(parse_fps("29.97").is_err());
        assert!(parse_fps("60/0").is_err());
    }

    #[test]
    fn colours_parse_with_and_without_the_hash() {
        assert_eq!(parse_color("101820").unwrap(), [0x10, 0x18, 0x20, 255]);
        assert_eq!(parse_color("#FF0080").unwrap(), [0xFF, 0x00, 0x80, 255]);
        assert!(parse_color("10182").is_err());
        assert!(parse_color("gggggg").is_err());
    }

    #[test]
    fn filters_parse_by_name() {
        assert_eq!(parse_filter("Point").unwrap(), ScaleFilter::Point);
        assert_eq!(parse_filter("nearest").unwrap(), ScaleFilter::Point);
        assert_eq!(parse_filter("bilinear").unwrap(), ScaleFilter::Bilinear);
        assert!(parse_filter("mitchell").is_err());
    }
}
