//! The Matroska writer: header once, then blocks as they come.

use std::io::{Seek, SeekFrom, Write};

use voltra_core::{
    AudioBuffer, ChannelLayout, Error, Fps, FrameSize, PixelFormat, Result, SampleRate, SampleSpec,
    Timestamp, VideoFrame,
};

use super::ebml::{self, UNKNOWN_SIZE, id};

/// Milliseconds: the unit `FFmpeg` writes and every player expects. Each block's
/// time is rounded from its own absolute timestamp, so the error is at most
/// half a millisecond and never accumulates.
const TIMESTAMP_SCALE_NS: u64 = 1_000_000;
/// A new cluster opens at the first keyframe this long after the current one.
const CLUSTER_SPAN_MS: u64 = 1_000;
/// And always before the 16-bit relative timestamp would overflow.
const CLUSTER_LIMIT_MS: u64 = 32_000;

/// One track of a recording.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TrackSpec {
    /// Uncompressed I420 video (`V_UNCOMPRESSED`, `FourCC` `I420`).
    RawVideo {
        /// Frame size; must be even in both dimensions.
        size: FrameSize,
        /// Frame rate, written as the track's default frame duration.
        fps: Fps,
    },
    /// 32-bit float PCM audio, interleaved (`A_PCM/FLOAT/IEEE`).
    PcmAudio {
        /// Sample rate.
        rate: SampleRate,
        /// Channel layout.
        layout: ChannelLayout,
    },
}

/// Writes a Matroska file as a stream of blocks.
///
/// Blocks must come in non-decreasing timestamp order across all tracks;
/// interleaving is the caller's job, because it knows what comes next. The
/// first block's time becomes the file's zero.
///
/// # Examples
///
/// ```
/// use voltra_core::{AudioBuffer, ChannelLayout, SampleRate, Timestamp};
/// use voltra_output::{MatroskaWriter, TrackSpec};
///
/// let tracks = [TrackSpec::PcmAudio { rate: SampleRate::HZ_48000, layout: ChannelLayout::Stereo }];
/// let mut writer = MatroskaWriter::new(Vec::new(), &tracks)?;
/// let block = AudioBuffer::new(SampleRate::HZ_48000, ChannelLayout::Stereo, 1024, Timestamp::ZERO)?;
/// writer.write_audio(0, &block)?;
/// let file = writer.finish()?;
/// assert_eq!(&file[..4], &[0x1A, 0x45, 0xDF, 0xA3], "EBML magic");
/// # Ok::<(), voltra_core::Error>(())
/// ```
#[derive(Debug)]
pub struct MatroskaWriter<W: Write> {
    sink: W,
    tracks: Vec<TrackSpec>,
    /// Bytes written so far, to find the duration placeholder again.
    written: u64,
    /// Where the 8 bytes of `Info/Duration` start, from the beginning.
    duration_at: u64,
    origin: Option<Timestamp>,
    last: Option<Timestamp>,
    /// The latest end time of any block, for the duration.
    end: Timestamp,
    /// The open cluster's timestamp, in milliseconds since the origin.
    cluster: Option<u64>,
    /// Reused for headers and PCM payloads: after the first blocks, writing
    /// does not allocate.
    scratch: Vec<u8>,
    header: Vec<u8>,
}

impl<W: Write> MatroskaWriter<W> {
    /// Start a file with these tracks, numbered from 0 in the calls below.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] for no tracks or an odd video size, [`Error::Io`]
    /// when the header cannot be written.
    pub fn new(sink: W, tracks: &[TrackSpec]) -> Result<Self> {
        if tracks.is_empty() {
            return Err(Error::config("a recording needs at least one track"));
        }
        let mut header = Vec::with_capacity(512);
        let duration_at = write_header(&mut header, tracks)?;
        let mut writer = Self {
            sink,
            tracks: tracks.to_vec(),
            written: 0,
            duration_at: duration_at as u64,
            origin: None,
            last: None,
            end: Timestamp::ZERO,
            cluster: None,
            scratch: Vec::new(),
            header: Vec::with_capacity(32),
        };
        writer.put(&header)?;
        Ok(writer)
    }

    /// Write one block: a packet of `track` at `pts`. This is the primitive
    /// encoders use; the payload goes to the sink as is.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] for an unknown track or a timestamp earlier than the
    /// previous block's, [`Error::Io`] from the sink.
    pub fn write_block(
        &mut self,
        track: usize,
        pts: Timestamp,
        keyframe: bool,
        payload: &[u8],
    ) -> Result<()> {
        self.begin_block(track, pts, keyframe, payload.len())?;
        self.put(payload)
    }

    /// Write one I420 frame of a [`TrackSpec::RawVideo`] track, plane by plane
    /// and row by row, leaving any stride padding behind.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when the track is not raw video or the frame is not
    /// I420 at its size; otherwise as [`write_block`](Self::write_block).
    pub fn write_video(&mut self, track: usize, frame: &VideoFrame, pts: Timestamp) -> Result<()> {
        let Some(TrackSpec::RawVideo { size, fps }) = self.tracks.get(track).copied() else {
            return Err(Error::config(format!("track {track} is not raw video")));
        };
        if frame.format() != PixelFormat::I420 || frame.size() != size {
            return Err(Error::config(format!(
                "track {track} takes I420 at {size}, got {} at {}",
                frame.format(),
                frame.size()
            )));
        }
        let bytes = (0..frame.plane_count())
            .filter_map(|plane| frame.plane_layout(plane))
            .map(|plane| plane.row_bytes() * plane.height() as usize)
            .sum();
        self.begin_block(track, pts, true, bytes)?;
        for plane in 0..frame.plane_count() {
            if let Some(rows) = frame.rows(plane) {
                for row in rows {
                    self.put(row)?;
                }
            }
        }
        self.extend(pts, fps.frame_duration());
        Ok(())
    }

    /// Write one block of a [`TrackSpec::PcmAudio`] track at its own
    /// timestamp, as interleaved little-endian float.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when the track is not PCM audio or the block's rate or
    /// layout differ; otherwise as [`write_block`](Self::write_block).
    pub fn write_audio(&mut self, track: usize, block: &AudioBuffer) -> Result<()> {
        let Some(TrackSpec::PcmAudio { rate, layout }) = self.tracks.get(track).copied() else {
            return Err(Error::config(format!("track {track} is not PCM audio")));
        };
        if block.rate() != rate || block.layout() != layout {
            return Err(Error::config(format!(
                "track {track} takes {rate} {layout}, got {} {}",
                block.rate(),
                block.layout()
            )));
        }
        let spec = SampleSpec::F32_INTERLEAVED;
        let mut payload = std::mem::take(&mut self.scratch);
        payload.resize(spec.byte_len(block.frames(), block.channel_count()), 0);
        let result = block
            .write_to(&mut payload, spec)
            .and_then(|()| self.write_block(track, block.pts(), true, &payload));
        self.scratch = payload;
        result?;
        self.extend(block.pts(), block.duration());
        Ok(())
    }

    /// Flush and hand back the sink. The file is complete and valid; its
    /// duration is not declared, since declaring it means going back.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when the flush fails.
    pub fn finish(mut self) -> Result<W> {
        self.sink.flush()?;
        Ok(self.sink)
    }

    /// Check and account for a block, open a cluster if it needs one, and
    /// write the block's header: everything but its payload.
    fn begin_block(
        &mut self,
        track: usize,
        pts: Timestamp,
        keyframe: bool,
        len: usize,
    ) -> Result<()> {
        if track >= self.tracks.len() {
            return Err(Error::config(format!("no track {track}")));
        }
        if self.last.is_some_and(|last| pts < last) {
            return Err(Error::config(format!(
                "block at {pts} after one at {}: blocks must come in time order",
                self.last.unwrap_or(pts)
            )));
        }
        self.last = Some(pts);
        let origin = *self.origin.get_or_insert(pts);
        let ms = to_ms(pts.since(origin));
        self.extend(pts, std::time::Duration::ZERO);

        let opens = match self.cluster {
            None => true,
            Some(start) => {
                let span = ms - start;
                (span >= CLUSTER_SPAN_MS && keyframe) || span >= CLUSTER_LIMIT_MS
            }
        };
        let mut header = std::mem::take(&mut self.header);
        header.clear();
        if opens {
            ebml::put_id(&mut header, id::CLUSTER);
            header.extend_from_slice(&UNKNOWN_SIZE);
            ebml::put_uint(&mut header, id::TIMESTAMP, ms);
            self.cluster = Some(ms);
        }
        let relative = ms - self.cluster.unwrap_or(ms);
        ebml::put_id(&mut header, id::SIMPLE_BLOCK);
        ebml::put_size(&mut header, 4 + len as u64);
        // Track number as a one-byte variable-length integer: tracks 1–126.
        ebml::put_size(&mut header, track as u64 + 1);
        #[allow(
            clippy::cast_possible_truncation,
            reason = "below CLUSTER_LIMIT_MS, within i16"
        )]
        header.extend_from_slice(&(relative as i16).to_be_bytes());
        header.push(if keyframe { 0x80 } else { 0x00 });
        let result = self.put(&header);
        self.header = header;
        result
    }

    /// Note that a block reaching `pts + duration` was written.
    fn extend(&mut self, pts: Timestamp, duration: std::time::Duration) {
        let end = Timestamp::from_nanos(
            pts.as_nanos()
                .saturating_add(u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)),
        );
        self.end = self.end.max(end);
    }

    fn put(&mut self, bytes: &[u8]) -> Result<()> {
        self.sink.write_all(bytes)?;
        self.written += bytes.len() as u64;
        Ok(())
    }
}

impl<W: Write + Seek> MatroskaWriter<W> {
    /// [`finish`](Self::finish), after writing the duration into the space
    /// reserved for it in the header.
    ///
    /// # Errors
    ///
    /// [`Error::Io`] when seeking or writing fails.
    pub fn finish_with_duration(mut self) -> Result<W> {
        let origin = self.origin.unwrap_or(Timestamp::ZERO);
        #[allow(
            clippy::cast_precision_loss,
            reason = "a duration in milliseconds as a float"
        )]
        let duration = self.end.since(origin) as f64 / TIMESTAMP_SCALE_NS as f64;
        let back = i64::try_from(self.written - self.duration_at).unwrap_or(i64::MAX);
        self.sink.seek(SeekFrom::Current(-back))?;
        self.sink.write_all(&duration.to_be_bytes())?;
        self.sink.seek(SeekFrom::Current(back - 8))?;
        self.finish()
    }
}

/// Nanoseconds to the file's milliseconds, rounding to the nearest.
const fn to_ms(nanos: u64) -> u64 {
    (nanos + TIMESTAMP_SCALE_NS / 2) / TIMESTAMP_SCALE_NS
}

/// EBML header, the start of an unknown-size `Segment`, `Info` and `Tracks`.
/// Returns where the `Duration` value starts.
fn write_header(out: &mut Vec<u8>, tracks: &[TrackSpec]) -> Result<usize> {
    let mut ebml_header = Vec::new();
    ebml::put_uint(&mut ebml_header, id::EBML_VERSION, 1);
    ebml::put_uint(&mut ebml_header, id::EBML_READ_VERSION, 1);
    ebml::put_uint(&mut ebml_header, id::EBML_MAX_ID_LENGTH, 4);
    ebml::put_uint(&mut ebml_header, id::EBML_MAX_SIZE_LENGTH, 8);
    ebml::put_bytes(&mut ebml_header, id::DOC_TYPE, b"matroska");
    ebml::put_uint(&mut ebml_header, id::DOC_TYPE_VERSION, 4);
    ebml::put_uint(&mut ebml_header, id::DOC_TYPE_READ_VERSION, 2);
    ebml::put_master(out, id::EBML, &ebml_header);

    ebml::put_id(out, id::SEGMENT);
    out.extend_from_slice(&UNKNOWN_SIZE);

    let app = concat!("Voltra Studio ", env!("CARGO_PKG_VERSION"));
    let mut info = Vec::new();
    ebml::put_uint(&mut info, id::TIMESTAMP_SCALE, TIMESTAMP_SCALE_NS);
    let duration_in_info = ebml::put_float(&mut info, id::DURATION, 0.0);
    ebml::put_bytes(&mut info, id::MUXING_APP, app.as_bytes());
    ebml::put_bytes(&mut info, id::WRITING_APP, app.as_bytes());
    // The Info header is its ID (4 bytes) and a size that fits one byte or two.
    let before_info = out.len();
    ebml::put_master(out, id::INFO, &info);
    let info_header = out.len() - before_info - info.len();
    let duration_at = before_info + info_header + duration_in_info;

    let mut entries = Vec::new();
    for (index, track) in tracks.iter().enumerate() {
        let mut entry = Vec::new();
        let number = index as u64 + 1;
        ebml::put_uint(&mut entry, id::TRACK_NUMBER, number);
        ebml::put_uint(&mut entry, id::TRACK_UID, number);
        ebml::put_uint(&mut entry, id::FLAG_LACING, 0);
        match *track {
            TrackSpec::RawVideo { size, fps } => {
                if size.width() % 2 != 0 || size.height() % 2 != 0 {
                    return Err(Error::config(format!(
                        "I420 needs an even size, not {size}"
                    )));
                }
                ebml::put_uint(&mut entry, id::TRACK_TYPE, 1);
                ebml::put_bytes(&mut entry, id::CODEC_ID, b"V_UNCOMPRESSED");
                let frame = u64::try_from(fps.frame_duration().as_nanos()).unwrap_or(u64::MAX);
                ebml::put_uint(&mut entry, id::DEFAULT_DURATION, frame);
                let mut video = Vec::new();
                ebml::put_uint(&mut video, id::PIXEL_WIDTH, u64::from(size.width()));
                ebml::put_uint(&mut video, id::PIXEL_HEIGHT, u64::from(size.height()));
                ebml::put_bytes(&mut video, id::COLOUR_SPACE, b"I420");
                ebml::put_master(&mut entry, id::VIDEO, &video);
            }
            TrackSpec::PcmAudio { rate, layout } => {
                ebml::put_uint(&mut entry, id::TRACK_TYPE, 2);
                ebml::put_bytes(&mut entry, id::CODEC_ID, b"A_PCM/FLOAT/IEEE");
                let mut audio = Vec::new();
                ebml::put_float(&mut audio, id::SAMPLING_FREQUENCY, f64::from(rate.hz()));
                ebml::put_uint(&mut audio, id::CHANNELS, layout.channels() as u64);
                ebml::put_uint(&mut audio, id::BIT_DEPTH, 32);
                ebml::put_master(&mut entry, id::AUDIO, &audio);
            }
        }
        ebml::put_master(&mut entries, id::TRACK_ENTRY, &entry);
    }
    ebml::put_master(out, id::TRACKS, &entries);
    Ok(duration_at)
}

#[cfg(test)]
mod tests;
