//! What every producer and processor of video has to implement.
//!
//! The shape follows `obs_source_info`: an identity string, a type, declared
//! capabilities, and a tick separate from producing output. Filters,
//! transitions and scenes are sources too, which is what gives libobs its
//! nesting for free and is worth adopting as it stands.
//!
//! # Only the asynchronous model lives here
//!
//! libobs offers two ways for a source to deliver video: the *asynchronous*
//! model, where the source pushes a frame from system memory
//! (`obs_source_output_video`), and the *render* model, where the source draws
//! itself when asked (`video_render`). Cameras and media files use the first;
//! screen capture and text use the second.
//!
//! `video_render` takes a `gs_effect_t*` — it is a graphics-backend callback.
//! Putting it here would make the shared vocabulary depend on the GPU, which
//! the structure rules forbid. So this module defines the asynchronous model,
//! and the render model becomes a separate trait in `voltra-render` beside the
//! backend. A source may implement either or both, as in OBS.

use std::sync::atomic::{AtomicU64, Ordering};

use crate::frame::{FrameSize, VideoFrame};
use crate::time::{Fps, Timestamp};

/// A stable handle to a source instance.
///
/// An identifier rather than a pointer: a scene refers to its sources by id, so
/// a source can be replaced or reloaded without every reference going stale.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SourceId(u64);

impl SourceId {
    /// Build an identifier from a raw value.
    ///
    /// Prefer [`SourceIdGenerator`]; this exists for deserialising a saved
    /// scene, where the identifiers already exist.
    pub const fn from_raw(value: u64) -> Self {
        SourceId(value)
    }

    /// The underlying value.
    pub const fn get(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for SourceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "source#{}", self.0)
    }
}

/// Hands out identifiers that are unique for the life of the process.
///
/// Thread-safe and lock-free: sources can be created from the UI thread while
/// the render thread is running.
#[derive(Debug, Default)]
pub struct SourceIdGenerator {
    next: AtomicU64,
}

impl SourceIdGenerator {
    /// A generator starting from one. Zero stays free as a sentinel.
    pub const fn new() -> Self {
        Self {
            next: AtomicU64::new(1),
        }
    }

    /// The next identifier.
    pub fn next_id(&self) -> SourceId {
        SourceId(self.next.fetch_add(1, Ordering::Relaxed))
    }
}

/// What role a source plays.
///
/// The same four libobs uses. Filters, transitions and scenes being sources is
/// what lets a scene contain a scene.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum SourceType {
    /// Produces content: a camera, a capture, a colour, a media file.
    Input,
    /// Processes the output of another source.
    Filter,
    /// Blends between two sources over time.
    Transition,
    /// Composites a collection of sources.
    Scene,
}

impl SourceType {
    /// A short, stable name.
    pub const fn name(self) -> &'static str {
        match self {
            SourceType::Input => "input",
            SourceType::Filter => "filter",
            SourceType::Transition => "transition",
            SourceType::Scene => "scene",
        }
    }
}

/// What a source can do.
///
/// libobs packs this into an `output_flags` bitmask, where nothing prevents
/// declaring a contradiction — asynchronous delivery without video, say. Here
/// the combinations that make sense have names, and the ones that do not cannot
/// be written.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SourceCapabilities {
    video: bool,
    audio: bool,
    composite: bool,
}

impl SourceCapabilities {
    /// Produces video only: a colour, a still image, a screen capture.
    pub const VIDEO: Self = Self {
        video: true,
        audio: false,
        composite: false,
    };

    /// Produces audio only: a microphone, a system capture.
    pub const AUDIO: Self = Self {
        video: false,
        audio: true,
        composite: false,
    };

    /// Produces both, already synchronised: a camera, a media file.
    pub const AUDIO_VIDEO: Self = Self {
        video: true,
        audio: true,
        composite: false,
    };

    /// Composites sub-sources: a scene or a transition.
    ///
    /// Implies both media types, because a scene carries whatever its children
    /// carry.
    pub const COMPOSITE: Self = Self {
        video: true,
        audio: true,
        composite: true,
    };

    /// Whether this source contributes video.
    pub const fn has_video(self) -> bool {
        self.video
    }

    /// Whether this source contributes audio.
    pub const fn has_audio(self) -> bool {
        self.audio
    }

    /// Whether this source composites other sources.
    pub const fn is_composite(self) -> bool {
        self.composite
    }
}

/// The timing handed to a source on every tick.
///
/// libobs passes `float seconds` elapsed since the previous frame. An
/// accumulated `f32` is exactly the drift the rational clock exists to prevent,
/// so this carries the exact presentation timestamp and the rational frame rate.
/// [`delta_nanos`](TickContext::delta_nanos) is a convenience; the truth is
/// [`pts`](TickContext::pts).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TickContext {
    frame_index: u64,
    fps: Fps,
}

impl TickContext {
    /// The context for a given frame index at a given rate.
    pub const fn new(frame_index: u64, fps: Fps) -> Self {
        Self { frame_index, fps }
    }

    /// The absolute frame number since the stream started.
    pub const fn frame_index(self) -> u64 {
        self.frame_index
    }

    /// The output frame rate.
    pub const fn fps(self) -> Fps {
        self.fps
    }

    /// The exact presentation timestamp of this frame.
    pub fn pts(self) -> Timestamp {
        self.fps.pts(self.frame_index)
    }

    /// Nanoseconds since the previous frame, computed from absolute timestamps
    /// so it carries no accumulated error.
    pub fn delta_nanos(self) -> u64 {
        let previous = self.fps.pts(self.frame_index.saturating_sub(1));
        self.pts().since(previous)
    }

    /// The context for the frame after this one.
    #[must_use]
    pub const fn next(self) -> Self {
        Self {
            frame_index: self.frame_index.saturating_add(1),
            fps: self.fps,
        }
    }
}

/// Common behaviour of everything in the source graph.
///
/// A failing source returns an error rather than logging and carrying on. That
/// is what lets the engine degrade one component instead of losing the
/// broadcast (CLAUDE.md §5).
pub trait Source {
    /// The stable type identifier, as `obs_source_info.id` is: `"color"`,
    /// `"pipewire_screen"`. Never translated, never displayed raw.
    fn kind(&self) -> &'static str;

    /// The role this source plays.
    fn source_type(&self) -> SourceType;

    /// What this source can do.
    fn capabilities(&self) -> SourceCapabilities;

    /// Advance the source to `context`.
    ///
    /// Called once per frame, before anything reads the source's output, so a
    /// source that changes its own state does it here and not mid-composition.
    ///
    /// # Errors
    ///
    /// Anything recoverable: a device that vanished, a file that ended. The
    /// engine records it as state and keeps running.
    fn tick(&mut self, context: &TickContext) -> crate::Result<()> {
        let _ = context;
        Ok(())
    }

    /// The source entered the program output.
    ///
    /// A capture may open its device here rather than while merely configured.
    fn activate(&mut self) {}

    /// The source left the program output.
    fn deactivate(&mut self) {}
}

/// A source that produces video frames in system memory.
///
/// The asynchronous model: the source owns its frame and hands out a reference.
/// Cameras, media files and anything that cannot control when frames arrive.
pub trait VideoSource: Source {
    /// The size of the frames this source produces.
    ///
    /// `None` before the first frame arrives. An asynchronous source genuinely
    /// does not know its resolution until the device says so, which is why
    /// `get_width` is optional for asynchronous sources in libobs.
    fn size(&self) -> Option<FrameSize>;

    /// The most recent frame, or `None` before the first one.
    ///
    /// Borrowed rather than cloned: a frame is megabytes, and CLAUDE.md §4.2
    /// forbids copying pixels on the frame path.
    fn frame(&self) -> Option<&VideoFrame>;
}

/// A source that transforms another source's frames.
pub trait VideoFilter: Source {
    /// Transform `frame` in place.
    ///
    /// In place, as `filter_video` is in libobs, which receives and returns the
    /// same frame. A filter needing a second buffer keeps its own
    /// [`FramePool`](crate::FramePool), so the cost sits with the filter that
    /// chose it rather than being charged to every filter.
    ///
    /// # Errors
    ///
    /// [`Error::Unsupported`](crate::Error::Unsupported) when the filter cannot
    /// work on that pixel format. Refusing beats writing something wrong.
    fn filter(&mut self, frame: &mut VideoFrame) -> crate::Result<()>;
}

#[cfg(test)]
mod tests {
    use super::{
        Source, SourceCapabilities, SourceId, SourceIdGenerator, SourceType, TickContext,
        VideoSource,
    };
    use crate::frame::{FrameSize, VideoFrame};
    use crate::pixel::PixelFormat;
    use crate::time::{Fps, Timestamp};

    /// The smallest thing that satisfies the traits, used to prove they are
    /// object safe and that the defaults behave.
    #[derive(Debug)]
    struct StubSource {
        frame: Option<VideoFrame>,
        ticks: u32,
        active: bool,
    }

    impl Source for StubSource {
        fn kind(&self) -> &'static str {
            "stub"
        }
        fn source_type(&self) -> SourceType {
            SourceType::Input
        }
        fn capabilities(&self) -> SourceCapabilities {
            SourceCapabilities::VIDEO
        }
        fn tick(&mut self, _context: &TickContext) -> crate::Result<()> {
            self.ticks += 1;
            Ok(())
        }
        fn activate(&mut self) {
            self.active = true;
        }
    }

    impl VideoSource for StubSource {
        fn size(&self) -> Option<FrameSize> {
            self.frame.as_ref().map(VideoFrame::size)
        }
        fn frame(&self) -> Option<&VideoFrame> {
            self.frame.as_ref()
        }
    }

    #[test]
    fn identifiers_are_unique_and_increasing() {
        let generator = SourceIdGenerator::new();
        let first = generator.next_id();
        let second = generator.next_id();
        assert!(second > first);
        assert_ne!(first, second);
        // Zero stays free as a sentinel.
        assert!(first.get() > 0);
        assert_eq!(SourceId::from_raw(7).get(), 7);
        assert_eq!(SourceId::from_raw(7).to_string(), "source#7");
    }

    #[test]
    fn capabilities_describe_coherent_combinations() {
        assert!(SourceCapabilities::VIDEO.has_video());
        assert!(!SourceCapabilities::VIDEO.has_audio());
        assert!(SourceCapabilities::AUDIO.has_audio());
        assert!(!SourceCapabilities::AUDIO.has_video());
        assert!(SourceCapabilities::AUDIO_VIDEO.has_video());
        assert!(SourceCapabilities::AUDIO_VIDEO.has_audio());
        // A composite carries whatever its children carry.
        assert!(SourceCapabilities::COMPOSITE.is_composite());
        assert!(SourceCapabilities::COMPOSITE.has_video());
        assert!(SourceCapabilities::COMPOSITE.has_audio());
    }

    /// The reason `TickContext` carries an index and a rational rate rather
    /// than accumulated seconds.
    #[test]
    fn tick_timing_is_exact_at_ntsc() {
        let context = TickContext::new(108_000, Fps::FPS_29_97);
        assert_eq!(context.pts(), Timestamp::from_nanos(3_603_600_000_000));
        assert_eq!(context.frame_index(), 108_000);
        assert_eq!(context.fps(), Fps::FPS_29_97);

        // The delta comes from absolute timestamps, so it never accumulates.
        let delta = context.delta_nanos();
        assert!(
            (33_366_000..=33_367_000).contains(&delta),
            "unexpected delta {delta}"
        );
    }

    #[test]
    fn the_first_frame_has_no_previous_one() {
        let first = TickContext::new(0, Fps::FPS_60);
        assert_eq!(first.pts(), Timestamp::ZERO);
        assert_eq!(first.delta_nanos(), 0);
        assert_eq!(first.next().frame_index(), 1);
    }

    #[test]
    fn the_traits_are_object_safe() {
        let size = FrameSize::new(16, 16).expect("valid size");
        let frame = VideoFrame::new(PixelFormat::Bgra8, size, Timestamp::ZERO).expect("frame");
        let mut source: Box<dyn VideoSource> = Box::new(StubSource {
            frame: Some(frame),
            ticks: 0,
            active: false,
        });

        source
            .tick(&TickContext::new(0, Fps::FPS_60))
            .expect("tick");
        source.activate();

        assert_eq!(source.kind(), "stub");
        assert_eq!(source.source_type(), SourceType::Input);
        assert_eq!(source.size(), Some(size));
        assert!(source.frame().is_some());
    }

    #[test]
    fn a_source_without_frames_reports_no_size() {
        let source = StubSource {
            frame: None,
            ticks: 0,
            active: false,
        };
        assert_eq!(source.size(), None);
        assert!(source.frame().is_none());
        assert_eq!(SourceType::Transition.name(), "transition");
    }
}
