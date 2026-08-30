//! End-to-end tests of `voltra render`.
//!
//! These run the real binary, because the thing being tested is the whole path
//! — argument parsing, scene, compositor, converter, writer, file — and any of
//! those can be right in isolation and wrong together.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::process::Command;

/// A file in the temporary directory, removed when the test finishes.
struct TempFile(PathBuf);

impl TempFile {
    fn new(name: &str) -> Self {
        let mut path = std::env::temp_dir();
        path.push(format!("voltra-{}-{name}.y4m", std::process::id()));
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }

    fn bytes(&self) -> Vec<u8> {
        std::fs::read(&self.0).expect("the rendered file")
    }
}

impl Drop for TempFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn render(output: &std::path::Path, extra: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_voltra"))
        .arg("render")
        .arg("-o")
        .arg(output)
        .args(extra)
        .output()
        .expect("running voltra")
}

/// The header up to but not including its newline.
fn header_of(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == b'\n').expect("a header");
    String::from_utf8(bytes[..end].to_vec()).expect("ASCII header")
}

#[test]
fn a_rendered_file_has_the_exact_size_the_format_implies() {
    let file = TempFile::new("size");
    let output = render(file.path(), &["--size", "160x90", "--frames", "3"]);
    assert!(output.status.success(), "{output:?}");

    let bytes = file.bytes();
    let header = header_of(&bytes);
    assert_eq!(
        header,
        "YUV4MPEG2 W160 H90 F60:1 Ip A1:1 C420jpeg XYSCSS=420JPEG XCOLORRANGE=LIMITED"
    );

    // Header and its newline, then three times `FRAME\n` plus a 4:2:0 frame.
    let expected = header.len() + 1 + 3 * (6 + 160 * 90 * 3 / 2);
    assert_eq!(bytes.len(), expected);
}

/// The demonstration scene is a pure function of the frame index, which is what
/// makes a rendered file something a test can compare against.
#[test]
fn two_runs_produce_identical_files() {
    let first = TempFile::new("determinism-a");
    let second = TempFile::new("determinism-b");
    let args = ["--size", "160x90", "--frames", "5", "--fps", "30"];

    assert!(render(first.path(), &args).status.success());
    assert!(render(second.path(), &args).status.success());
    assert_eq!(first.bytes(), second.bytes());
}

/// Rendering must not be silent about what it cost: the summary goes to stderr
/// so that stdout stays a clean video pipe.
#[test]
fn the_cost_summary_goes_to_stderr() {
    let file = TempFile::new("metrics");
    let output = render(file.path(), &["--size", "160x90", "--frames", "2"]);
    let stderr = String::from_utf8(output.stderr).expect("UTF-8 diagnostics");

    assert!(stderr.contains("rendered 2 frames of 160x90"), "{stderr}");
    for stage in ["composite", "convert", "write", "total"] {
        assert!(stderr.contains(stage), "no `{stage}` line in:\n{stderr}");
    }
}

/// The rational clock has to survive all the way to the file: 29.97 is
/// 30000/1001, not a decimal Y4M has no way to write.
#[test]
fn drop_frame_rates_reach_the_file_as_ratios() {
    let file = TempFile::new("ntsc");
    let output = render(
        file.path(),
        &["--size", "160x90", "--frames", "1", "--fps", "30000/1001"],
    );
    assert!(output.status.success(), "{output:?}");
    assert!(header_of(&file.bytes()).contains(" F30000:1001 "));
}

#[test]
fn full_range_is_declared_in_the_header() {
    let file = TempFile::new("full-range");
    let output = render(
        file.path(),
        &["--size", "160x90", "--frames", "1", "--full-range"],
    );
    assert!(output.status.success(), "{output:?}");
    assert!(header_of(&file.bytes()).ends_with(" XCOLORRANGE=FULL"));
}

/// 4:2:0 cannot describe half a row, so an odd canvas is refused at the command
/// line rather than half-way through a file.
#[test]
fn odd_canvas_sizes_are_refused_before_anything_is_written() {
    let file = TempFile::new("odd");
    let output = render(file.path(), &["--size", "161x90", "--frames", "1"]);

    assert!(!output.status.success());
    assert!(!file.path().exists(), "a refused run must leave no file");
}

/// `-o -` is what makes `voltra render -o - | ffplay -` work, so the video has
/// to arrive on stdout and nothing else may.
#[test]
fn the_stream_can_go_to_stdout() {
    let output = Command::new(env!("CARGO_BIN_EXE_voltra"))
        .args(["render", "-o", "-", "--size", "160x90", "--frames", "2"])
        .output()
        .expect("running voltra");

    assert!(output.status.success(), "{output:?}");
    assert!(output.stdout.starts_with(b"YUV4MPEG2 W160 H90 "));
    assert_eq!(
        output.stdout.len(),
        header_of(&output.stdout).len() + 1 + 2 * (6 + 160 * 90 * 3 / 2)
    );
}
