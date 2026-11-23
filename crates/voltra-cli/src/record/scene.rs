//! What gets recorded: the demo scene with a white frame at each second, and a
//! tone through the mixer that sounds at each second.

use anyhow::{Context, Result};
use voltra_audio::{MixSpec, Mixer};
use voltra_core::{
    AudioBuffer, ChannelLayout, ColorRange, ColorSpace, ColorSpec, Fps, FrameSize, PixelFormat,
    SampleRate, ScaleFilter, TickContext, Timestamp, VideoFrame, rgb_to_yuv,
};
use voltra_render::{Compositor, CpuCompositor};

use crate::demo::DemoScene;

/// The mix rate and block size the recording uses.
pub(super) const RATE: SampleRate = SampleRate::HZ_48000;
pub(super) const BLOCK: usize = 1024;
/// How long each second's tone lasts.
const MARK: std::time::Duration = std::time::Duration::from_millis(50);

/// The scene, composited and converted, with a white frame at each second.
pub(super) struct Video {
    demo: DemoScene,
    compositor: CpuCompositor,
    frame: VideoFrame,
    spec: ColorSpec,
    context: TickContext,
}

impl Video {
    pub(super) fn new(size: FrameSize, fps: Fps) -> Result<Self> {
        let mut compositor = CpuCompositor::new(size).context("building the compositor")?;
        compositor.set_background([0x10, 0x18, 0x20, 0xFF]);
        Ok(Self {
            demo: DemoScene::new(size, ScaleFilter::Bilinear).context("building the demo scene")?,
            compositor,
            frame: VideoFrame::new(PixelFormat::I420, size, Timestamp::ZERO)
                .context("allocating the frame")?,
            spec: ColorSpec::new(ColorSpace::Bt709, ColorRange::Limited),
            context: TickContext::new(0, fps),
        })
    }

    /// Frame `n`, stamped `pts`: the first frame at or after a whole second of
    /// the recording is a sync mark.
    pub(super) fn frame(&mut self, n: u64, fps: Fps, pts: Timestamp) -> Result<&VideoFrame> {
        self.demo.tick(&self.context).context("ticking the scene")?;
        let canvas = self
            .compositor
            .composite(self.demo.scene(), &self.demo, fps.pts(n))
            .context("compositing")?;
        rgb_to_yuv(canvas, &mut self.frame, self.spec).context("converting to I420")?;
        // After the conversion, which carries the canvas's own timestamp over:
        // the recording's timeline starts at its origin, not at zero.
        self.frame.set_pts(pts);
        if is_mark_frame(n, fps) {
            // Limited-range white, neutral chroma.
            for (plane, value) in [(0, 235u8), (1, 128), (2, 128)] {
                if let Some(bytes) = self.frame.plane_mut(plane) {
                    bytes.fill(value);
                }
            }
        }
        self.context = self.context.next();
        Ok(&self.frame)
    }
}

/// Whether frame `n` is the first at or after a whole second: some second
/// `s` lies in `(pts(n − 1), pts(n)]`, which is when the whole seconds elapsed
/// — rounded down — differ between the two frames.
pub(super) fn is_mark_frame(n: u64, fps: Fps) -> bool {
    let second = |n: u64| fps.pts(n).as_nanos() / 1_000_000_000;
    n == 0 || second(n) != second(n - 1)
}

/// A mono tone that sounds for [`MARK`] at each second, through the mixer.
pub(super) struct Audio {
    mixer: Mixer,
    track: voltra_audio::TrackId,
    tone: AudioBuffer,
    mix: AudioBuffer,
    hz: f64,
}

impl Audio {
    pub(super) fn new(hz: f64) -> Result<Self> {
        let spec = MixSpec::new(RATE, ChannelLayout::Stereo, BLOCK).context("the mix")?;
        let mut mixer = Mixer::new(spec).context("building the mixer")?;
        let track = mixer
            .add_track(ChannelLayout::Mono)
            .context("adding the tone")?
            .id();
        Ok(Self {
            mixer,
            track,
            tone: AudioBuffer::new(RATE, ChannelLayout::Mono, BLOCK, Timestamp::ZERO)
                .context("allocating the tone")?,
            mix: spec.buffer(Timestamp::ZERO).context("allocating the mix")?,
            hz,
        })
    }

    /// Mix block `k` of the recording, stamped `pts`.
    pub(super) fn block(&mut self, k: u64, pts: Timestamp) -> Result<&AudioBuffer> {
        let first = k * BLOCK as u64;
        let rate = u64::from(RATE.hz());
        let mark = RATE.frames_in(MARK);
        if let Some(samples) = self.tone.channel_mut(0) {
            for (offset, sample) in samples.iter_mut().enumerate() {
                let index = first + offset as u64;
                *sample = if index % rate < mark {
                    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)]
                    let value =
                        (std::f64::consts::TAU * self.hz * index as f64 / rate as f64).sin() as f32;
                    0.5 * value
                } else {
                    0.0
                };
            }
        }
        self.tone.set_pts(pts);
        self.mix.set_pts(pts);
        self.mixer
            .mix([(self.track, &self.tone)], &mut self.mix)
            .context("mixing")?;
        Ok(&self.mix)
    }
}

#[cfg(test)]
mod tests {
    use voltra_core::Fps;

    use super::is_mark_frame;

    #[test]
    fn the_mark_is_the_first_frame_of_each_second() {
        let marks: Vec<u64> = (0..100)
            .filter(|&n| is_mark_frame(n, Fps::FPS_30))
            .collect();
        assert_eq!(marks, [0, 30, 60, 90]);
        let ntsc = Fps::new(30_000, 1_001).unwrap();
        let marks: Vec<u64> = (0..100).filter(|&n| is_mark_frame(n, ntsc)).collect();
        // 29.97 fps: frame 30 starts at 1.001 s, the first at or after 1 s.
        assert_eq!(marks, [0, 30, 60, 90]);
    }
}
