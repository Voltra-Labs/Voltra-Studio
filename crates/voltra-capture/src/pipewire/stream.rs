//! The capture thread: a PipeWire loop with one input stream.

use std::sync::atomic::Ordering;

use pipewire as pw;
use pw::spa;
use pw::spa::buffer::meta::{MetaHeader, MetaHeaderFlags};
use pw::spa::pod::{Object, Pod, Property, PropertyFlags, Value};
use pw::spa::utils::{Id, SpaTypes};
use voltra_core::{Error, FrameSize, PixelFormat, Result};

use super::{Counters, PipeWireTarget};
use crate::clock::{ClockMap, process_time};
use crate::convert::{SourceFormat, copy_to_bgra};
use crate::delivery::FrameWriter;
use crate::source::{CaptureStatus, StatusCell};

/// What the stream's callbacks share.
struct State<'a> {
    writer: FrameWriter,
    status: &'a StatusCell,
    counters: &'a Counters,
    format: Option<(SourceFormat, FrameSize)>,
    clock: ClockMap,
}

/// Run the capture until `quit` receives, or the stream fails.
pub(super) fn run(
    target: &PipeWireTarget,
    writer: FrameWriter,
    quit: pw::channel::Receiver<()>,
    status: &StatusCell,
    counters: &Counters,
) -> Result<()> {
    pw::init();
    let mainloop = pw::main_loop::MainLoopRc::new(None).map_err(native)?;
    let context = pw::context::ContextRc::new(&mainloop, None).map_err(native)?;
    let remote = target.remote.as_deref().map(|remote| {
        pw::properties::properties! { *pw::keys::REMOTE_NAME => remote }
    });
    let core = context.connect_rc(remote).map_err(native)?;

    let _quit = quit.attach(mainloop.loop_(), {
        let mainloop = mainloop.clone();
        move |()| mainloop.quit()
    });

    let stream = pw::stream::StreamBox::new(
        &core,
        &target.name,
        pw::properties::properties! {
            *pw::keys::MEDIA_TYPE => "Video",
            *pw::keys::MEDIA_CATEGORY => "Capture",
            *pw::keys::MEDIA_ROLE => "Screen",
            *pw::keys::NODE_NAME => target.name.as_str(),
        },
    )
    .map_err(native)?;

    let state = State {
        writer,
        status,
        counters,
        format: None,
        clock: ClockMap::default(),
    };
    let _listener = stream
        .add_local_listener_with_user_data(state)
        .state_changed({
            let mainloop = mainloop.clone();
            move |_, state, _, new| {
                if let pw::stream::StreamState::Error(message) = new {
                    tracing::error!("PipeWire stream error: {message}");
                    state.status.set(CaptureStatus::Failed);
                    mainloop.quit();
                }
            }
        })
        .param_changed(|stream, state, id, param| {
            if id != spa::param::ParamType::Format.as_raw() {
                return;
            }
            let Some(param) = param else {
                return;
            };
            state.format = negotiated(param);
            // Ask for the header, which carries each buffer's timestamp.
            if let Ok(bytes) = serialize(&meta_header()) {
                if let Some(pod) = Pod::from_bytes(&bytes) {
                    let _ = stream.update_params(&mut [pod]);
                }
            }
        })
        .process(|stream, state| process(stream, state))
        .register()
        .map_err(native)?;

    let format = serialize(&enum_format())?;
    let mut params = [Pod::from_bytes(&format).ok_or_else(|| native("format pod"))?];
    let mut flags = pw::stream::StreamFlags::MAP_BUFFERS;
    if target.autoconnect {
        flags |= pw::stream::StreamFlags::AUTOCONNECT;
    }
    stream
        .connect(
            spa::utils::Direction::Input,
            target.node,
            flags,
            &mut params,
        )
        .map_err(native)?;

    mainloop.run();
    let _ = stream.disconnect();
    Ok(())
}

/// One `process` callback: drain to the newest buffer, convert, publish.
fn process(stream: &pw::stream::Stream, state: &mut State<'_>) {
    let mut newest = None;
    while let Some(buffer) = stream.dequeue_buffer() {
        // Dropping the earlier one hands it back to PipeWire unused.
        if newest.replace(buffer).is_some() {
            state
                .counters
                .source_dropped
                .fetch_add(1, Ordering::Relaxed);
        }
    }
    let Some(mut buffer) = newest else {
        return;
    };
    let Some((format, size)) = state.format else {
        return;
    };
    let header = buffer
        .find_meta::<MetaHeader>()
        .map(|header| (header.pts(), header.flags()));
    let corrupted = header.is_some_and(|(_, flags)| flags.contains(MetaHeaderFlags::CORRUPTED));

    let process_now = process_time();
    let pts = match header {
        Some((pts, _)) if pts > 0 => match stream.time() {
            Ok(time) => state.clock.map(pts, time.now(), process_now),
            Err(_) => process_now,
        },
        _ => process_now,
    };
    state
        .counters
        .anchors
        .store(state.clock.anchors(), Ordering::Relaxed);

    let Some(data) = buffer.datas_mut().first_mut() else {
        state.counters.skipped.fetch_add(1, Ordering::Relaxed);
        return;
    };
    let chunk = data.chunk();
    let (offset, len) = (chunk.offset() as usize, chunk.size() as usize);
    let chunk_corrupted = chunk.flags().contains(spa::buffer::ChunkFlags::CORRUPTED);
    let stride = usize::try_from(chunk.stride()).unwrap_or(0);
    let stride = if stride == 0 {
        size.width() as usize * 4
    } else {
        stride
    };
    let Some(bytes) = data
        .data()
        .and_then(|bytes| bytes.get(offset..offset + len))
    else {
        state.counters.skipped.fetch_add(1, Ordering::Relaxed);
        return;
    };
    if corrupted || chunk_corrupted || bytes.is_empty() {
        state.counters.skipped.fetch_add(1, Ordering::Relaxed);
        return;
    }
    let Ok(frame) = state.writer.frame_mut(PixelFormat::Bgra8, size) else {
        state.counters.skipped.fetch_add(1, Ordering::Relaxed);
        return;
    };
    copy_to_bgra(bytes, stride, format, frame);
    state.writer.publish(pts);
    if state.status.get() == CaptureStatus::Starting {
        state.status.set(CaptureStatus::Running);
    }
}

/// The format and size PipeWire settled on, if it is one this backend reads.
fn negotiated(param: &Pod) -> Option<(SourceFormat, FrameSize)> {
    let (media_type, media_subtype) = spa::param::format_utils::parse_format(param).ok()?;
    if media_type != spa::param::format::MediaType::Video
        || media_subtype != spa::param::format::MediaSubtype::Raw
    {
        return None;
    }
    let mut info = spa::param::video::VideoInfoRaw::default();
    info.parse(param).ok()?;
    let format = match info.format() {
        spa::param::video::VideoFormat::BGRx => SourceFormat::Bgrx,
        spa::param::video::VideoFormat::BGRA => SourceFormat::Bgra,
        spa::param::video::VideoFormat::RGBx => SourceFormat::Rgbx,
        spa::param::video::VideoFormat::RGBA => SourceFormat::Rgba,
        _ => return None,
    };
    let size = FrameSize::new(info.size().width, info.size().height).ok()?;
    Some((format, size))
}

/// The formats offered: raw video in the four 8-bit layouts OBS offers, any
/// size up to 8K, any rate.
fn enum_format() -> Object {
    use spa::param::format::{FormatProperties, MediaSubtype, MediaType};
    use spa::param::video::VideoFormat;
    use spa::utils::{Fraction, Rectangle};
    spa::pod::object!(
        SpaTypes::ObjectParamFormat,
        spa::param::ParamType::EnumFormat,
        spa::pod::property!(FormatProperties::MediaType, Id, MediaType::Video),
        spa::pod::property!(FormatProperties::MediaSubtype, Id, MediaSubtype::Raw),
        spa::pod::property!(
            FormatProperties::VideoFormat,
            Choice,
            Enum,
            Id,
            VideoFormat::BGRx,
            VideoFormat::BGRx,
            VideoFormat::BGRA,
            VideoFormat::RGBx,
            VideoFormat::RGBA
        ),
        spa::pod::property!(
            FormatProperties::VideoSize,
            Choice,
            Range,
            Rectangle,
            Rectangle {
                width: 1920,
                height: 1080
            },
            Rectangle {
                width: 1,
                height: 1
            },
            Rectangle {
                width: 8192,
                height: 8192
            }
        ),
        spa::pod::property!(
            FormatProperties::VideoFramerate,
            Choice,
            Range,
            Fraction,
            Fraction { num: 60, denom: 1 },
            Fraction { num: 0, denom: 1 },
            Fraction {
                num: 1000,
                denom: 1
            }
        ),
    )
}

/// The `Meta` param asking for a `spa_meta_header` on every buffer.
fn meta_header() -> Object {
    let size = i32::try_from(size_of::<spa::sys::spa_meta_header>()).unwrap_or(0);
    Object {
        type_: SpaTypes::ObjectParamMeta.as_raw(),
        id: spa::param::ParamType::Meta.as_raw(),
        properties: vec![
            Property {
                key: spa::sys::SPA_PARAM_META_type,
                flags: PropertyFlags::empty(),
                value: Value::Id(Id(spa::sys::SPA_META_Header)),
            },
            Property {
                key: spa::sys::SPA_PARAM_META_size,
                flags: PropertyFlags::empty(),
                value: Value::Int(size),
            },
        ],
    }
}

fn serialize(object: &Object) -> Result<Vec<u8>> {
    spa::pod::serialize::PodSerializer::serialize(
        std::io::Cursor::new(Vec::new()),
        &Value::Object(object.clone()),
    )
    .map(|(cursor, _)| cursor.into_inner())
    .map_err(|error| native(format!("{error:?}")))
}

fn native(error: impl std::fmt::Display) -> Error {
    Error::unsupported(format!("PipeWire: {error}"))
}
