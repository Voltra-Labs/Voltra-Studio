//! The `record` subcommand: the scene and a sound, together, in one Matroska
//! file.
//!
//! ```text
//! voltra record -o demo.mkv --seconds 10
//! voltra record -o - | ffplay -
//! ```
//!
//! Video is the demonstration scene, as `voltra render` composites it: raw
//! I420, or H.264 through x264 (`--video h264`). Audio is a mono tone through a
//! track of the mixer into a 48 kHz stereo mix: float PCM, or AAC
//! (`--audio aac`). The encoders need a build with `--features ffmpeg`.
//!
//! **Sync marks.** At every whole second the frame that starts there is white,
//! and the tone sounds for 50 ms from that second's exact sample. Everything
//! else is silence. A tool that has never heard of Voltra can then measure the
//! audio/video sync in the file, which is what the tests do with `FFmpeg`.
//!
//! **The audio is the clock** (CLAUDE.md §5). Block `k` and frame `n` are
//! stamped from their absolute index on one timeline, made in time order, and
//! written through an interleaver, because encoders hand packets over late.
//! The file's zero is set explicitly, so an encoder's priming is written before
//! it, with the track's `CodecDelay`, and never becomes an A/V offset.

mod args;
mod pipeline;
mod report;
mod scene;

use std::time::Instant;

use anyhow::{Context, Result};

pub use args::Args;
use pipeline::{Encoders, record};
use voltra_output::MatroskaWriter;

/// Record and write the file.
///
/// # Errors
///
/// When an encoder cannot be opened, the destination cannot be opened or
/// written, or compositing, converting, mixing or encoding fails.
pub fn run(args: &Args) -> Result<()> {
    let mut encoders = Encoders::open(args)?;
    let tracks = encoders.tracks(args)?;
    let started = Instant::now();
    let summary = if args.output.as_os_str() == "-" {
        let sink = std::io::stdout().lock();
        let writer = MatroskaWriter::new(sink, &tracks).context("writing the header")?;
        let (writer, summary) = record(writer, args, &mut encoders)?;
        drop(writer.finish().context("flushing the recording")?);
        summary
    } else {
        let file = std::fs::File::create(&args.output)
            .with_context(|| format!("creating {}", args.output.display()))?;
        let sink = std::io::BufWriter::with_capacity(64 * 1024, file);
        let writer = MatroskaWriter::new(sink, &tracks).context("writing the header")?;
        let (writer, summary) = record(writer, args, &mut encoders)?;
        writer
            .finish_with_duration()
            .context("finishing the recording")?;
        summary
    };
    eprintln!(
        "recorded {} s: {} video and {} audio packets, at most {} queued, in {:.2} s",
        args.seconds,
        summary.video_packets,
        summary.audio_packets,
        summary.most_queued,
        started.elapsed().as_secs_f64()
    );
    let heap = crate::heap::stats();
    // The resident size now, not `VmHWM`: Linux updates its high-water mark
    // lazily, and it can read below a resident size reported earlier.
    let resident = crate::heap::resident(&mut String::new()).map_or(0, |resident| resident.now_kib);
    eprintln!(
        "heap: {} KiB live in {} blocks, {} KiB at most; {} KiB resident",
        heap.live_bytes / 1024,
        heap.live_blocks,
        heap.peak_bytes / 1024,
        resident
    );
    Ok(())
}
