//! Capture from a real PipeWire server: what arrives, what is lost on the way,
//! and with what timestamps.
//!
//! The test brings up everything it needs, in a runtime directory of its own:
//! a `pipewire` server — no session manager, no D-Bus, no display — a 60 fps
//! test source published by GStreamer, and a `pw-link` between the source and
//! the capture. It is skipped, and says so, when any of the three tools is not
//! installed.

#![cfg(feature = "pipewire")]
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::cast_precision_loss)]

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use voltra_capture::{CaptureStatus, PipeWireCapture, PipeWireTarget};
use voltra_core::{Fps, FrameSize, PixelFormat, Source, TickContext, VideoSource};

const SOURCE_NAME: &str = "voltra-test-source";
const CAPTURE_NAME: &str = "voltra-capture-test";

fn have(tool: &str) -> bool {
    let found = Command::new("sh")
        .args(["-c", &format!("command -v {tool}")])
        .output()
        .is_ok_and(|out| out.status.success());
    if !found {
        eprintln!("skipping: {tool} is not installed");
    }
    found
}

/// A PipeWire server and a test source, torn down on drop.
struct Server {
    dir: PathBuf,
    children: Vec<Child>,
}

impl Server {
    fn start() -> Self {
        // A Unix socket path cannot pass 108 bytes, so the directory is short.
        let dir = PathBuf::from(format!("/tmp/voltra-pw-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut server = Self {
            dir,
            children: Vec::new(),
        };
        let pipewire = server.command("pipewire").spawn().unwrap();
        server.children.push(pipewire);
        Server::wait_for(|| server.socket().exists(), "the server's socket");

        let producer = server
            .command("gst-launch-1.0")
            .args(["-q", "videotestsrc", "is-live=true", "pattern=ball", "!"])
            .arg("video/x-raw,format=BGRx,width=320,height=180,framerate=60/1")
            .args(["!", "pipewiresink", "mode=provide"])
            .arg(format!(
                "stream-properties=props,media.class=Video/Source,node.name={SOURCE_NAME}"
            ))
            .spawn()
            .unwrap();
        server.children.push(producer);
        server
    }

    fn socket(&self) -> PathBuf {
        self.dir.join("pipewire-0")
    }

    fn command(&self, program: &str) -> Command {
        let mut command = Command::new(program);
        command
            .env("XDG_RUNTIME_DIR", &self.dir)
            .env_remove("DBUS_SESSION_BUS_ADDRESS")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        command
    }

    fn wait_for(mut ready: impl FnMut() -> bool, what: &str) {
        let deadline = Instant::now() + Duration::from_secs(10);
        while !ready() {
            assert!(Instant::now() < deadline, "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// The ports `pw-link` lists in one direction (`-o` or `-i`).
    fn ports(&self, direction: &str) -> Vec<String> {
        let out = self
            .command("pw-link")
            .arg(direction)
            .stdout(Stdio::piped())
            .output()
            .unwrap();
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(str::to_owned)
            .collect()
    }

    /// Link the source's output port to the capture's input port, once both
    /// exist.
    fn link(&self) {
        let find = |ports: Vec<String>, node: &str| {
            ports
                .into_iter()
                .find(|port| port.starts_with(&format!("{node}:")))
        };
        let mut output = None;
        let mut input = None;
        Self::wait_for(
            || {
                output = output
                    .take()
                    .or_else(|| find(self.ports("-o"), SOURCE_NAME));
                input = input
                    .take()
                    .or_else(|| find(self.ports("-i"), CAPTURE_NAME));
                output.is_some() && input.is_some()
            },
            "both ports",
        );
        let status = self
            .command("pw-link")
            .args([output.unwrap(), input.unwrap()])
            .status()
            .unwrap();
        assert!(status.success(), "pw-link failed");
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        for child in self.children.iter_mut().rev() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

#[test]
fn a_60_fps_source_arrives_whole_and_on_time() {
    if !have("pipewire") || !have("gst-launch-1.0") || !have("pw-link") {
        return;
    }
    let server = Server::start();
    let mut source = PipeWireCapture::start(PipeWireTarget {
        node: None,
        autoconnect: false,
        name: CAPTURE_NAME.to_owned(),
        remote: Some(server.socket().display().to_string()),
    })
    .unwrap();
    assert_eq!(source.kind(), "pipewire_capture");
    server.link();
    Server::wait_for(
        || source.status() == CaptureStatus::Running,
        "the first frame",
    );

    // A render loop at 60 fps for 2 s, noting every new frame's timestamp.
    let fps = Fps::FPS_60;
    let before = source.stats();
    let start = Instant::now();
    let mut stamps = Vec::new();
    for n in 0..120u64 {
        source.tick(&TickContext::new(n, fps)).unwrap();
        let pts = source.frame().unwrap().pts();
        if stamps.last() != Some(&pts) {
            stamps.push(pts);
        }
        let deadline = start + Duration::from_nanos(fps.pts(n + 1).as_nanos());
        if let Some(wait) = deadline.checked_duration_since(Instant::now()) {
            std::thread::sleep(wait);
        }
    }
    let frame = source.frame().unwrap();
    assert_eq!(frame.size(), FrameSize::new(320, 180).unwrap());
    assert_eq!(frame.format(), PixelFormat::Bgra8);
    // `videotestsrc` paints opaque pixels, and BGRx arrives with alpha set.
    assert!(
        frame
            .as_bytes()
            .chunks_exact(4)
            .all(|pixel| pixel[3] == 0xFF)
    );

    source.stop();
    assert_eq!(source.status(), CaptureStatus::Stopped);
    let stats = source.stats();
    let backend = source.backend_stats();
    let captured = stats.captured - before.captured;
    eprintln!("2 s of a 60 fps PipeWire source: {stats:?}, {backend:?}");
    assert!((110..=130).contains(&captured), "{captured} frames in 2 s");
    assert_eq!(
        stats.shown + stats.dropped + stats.pending(),
        stats.captured
    );
    assert_eq!(backend.skipped, 0, "{backend:?}");

    // The source's own timestamps, on the process's clock: 16.7 ms apart on
    // average, and never going back.
    assert!(stamps.windows(2).all(|pair| pair[1] > pair[0]));
    let span = stamps.last().unwrap().since(*stamps.first().unwrap());
    let spacing = span as f64 / (stamps.len() - 1) as f64 / 1e6;
    eprintln!(
        "{} distinct frames shown, {spacing:.2} ms apart, clock anchored {} time(s)",
        stamps.len(),
        backend.clock_anchors
    );
    assert!(
        (15.0..=18.5).contains(&spacing),
        "{spacing} ms between frames"
    );
}
