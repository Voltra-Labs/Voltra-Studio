//! The `info` subcommand: what this build is and what it is running on.

use std::io::Write;

/// Write build and host information to `out`.
///
/// Takes a writer rather than printing directly so the output can be asserted
/// in tests without capturing stdout.
pub fn print(out: &mut impl Write) -> std::io::Result<()> {
    let parallelism = std::thread::available_parallelism().map_or_else(
        |_| "unknown".to_owned(),
        |threads| threads.get().to_string(),
    );
    let profile = if cfg!(debug_assertions) {
        "debug"
    } else {
        "release"
    };

    let rows = [
        ("version", voltra_core::VERSION.to_owned()),
        ("msrv", voltra_core::MSRV.to_owned()),
        ("profile", profile.to_owned()),
        (
            "host",
            format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
        ),
        ("parallelism", parallelism),
    ];

    writeln!(out, "Voltra Studio")?;
    for (key, value) in rows {
        writeln!(out, "  {key:<12} {value}")?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::print;

    #[test]
    fn reports_version_and_host() {
        let mut buffer = Vec::new();
        print(&mut buffer).expect("writing to a Vec cannot fail");
        let text = String::from_utf8(buffer).expect("output is valid UTF-8");

        assert!(text.contains("Voltra Studio"));
        assert!(text.contains(voltra_core::VERSION));
        assert!(text.contains(std::env::consts::ARCH));
        assert!(text.contains("parallelism"));
    }
}
