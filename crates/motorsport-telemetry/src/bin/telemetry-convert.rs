//! Convert any supported telemetry file to `.telemetry` (or `.mtj`/`.mtjx`).
//!
//! By default the conversion also runs the standard processing-pass registry
//! ([`telemetry_passes::registry`]) and appends the derived channels it
//! produces. Every pass is lossless: the original channels are stored
//! untouched, and `--strip-passes` recovers a byte-identical raw conversion.
//!
//! Usage: `telemetry-convert [--no-passes | --strip-passes] INPUT [OUTPUT]`
//!
//! Pass reports go to stderr, one line per registered pass; the destination
//! path is printed to stdout last.

use motorsport_telemetry::{open, TelemetryError};
use std::path::{Path, PathBuf};
use telemetry_format::{
    is_jsonl_ext_path, is_jsonl_path, write_from_source, write_from_source_stripped,
    write_jsonl_extension_from_source, write_jsonl_from_source,
};
use telemetry_passes::{apply_registry, PassOutcome};

/// What to do with the processing-pass registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PassMode {
    /// Run every applicable registered pass and append its channels.
    Apply,
    /// Convert the source as-is, applying nothing.
    Skip,
    /// Drop previously applied pass outputs, recovering the raw conversion.
    Strip,
}

fn default_dest(src: &Path) -> PathBuf {
    let mut dest = src.to_path_buf();
    let name = src
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("recording");
    dest.set_file_name(format!("{name}.telemetry"));
    dest
}

fn usage() -> TelemetryError {
    TelemetryError::Unsupported(
        "usage: telemetry-convert [--no-passes | --strip-passes] INPUT [OUTPUT]".into(),
    )
}

fn write_out(
    source: &dyn motorsport_telemetry_core::TelemetrySource,
    dest: &Path,
) -> Result<(), TelemetryError> {
    if is_jsonl_ext_path(dest) {
        write_jsonl_extension_from_source(source, dest)?;
    } else if is_jsonl_path(dest) {
        write_jsonl_from_source(source, dest)?;
    } else {
        write_from_source(source, dest)?;
    }
    Ok(())
}

fn main() -> Result<(), TelemetryError> {
    let mut mode = PassMode::Apply;
    let mut paths = Vec::new();
    for arg in std::env::args_os().skip(1) {
        match arg.to_str() {
            Some("--no-passes") => mode = PassMode::Skip,
            Some("--strip-passes") => mode = PassMode::Strip,
            Some(flag) if flag.starts_with("--") => return Err(usage()),
            _ => paths.push(PathBuf::from(arg)),
        }
    }
    let mut paths = paths.into_iter();
    let src = paths.next().ok_or_else(usage)?;
    let dest = paths.next().unwrap_or_else(|| default_dest(&src));
    if paths.next().is_some() {
        return Err(usage());
    }

    let file = open(&src)?;
    match mode {
        PassMode::Apply => {
            let (passed, reports) = apply_registry(&file)
                .map_err(|error| TelemetryError::Unsupported(error.to_string()))?;
            for report in &reports {
                match &report.outcome {
                    PassOutcome::Applied { outputs } => {
                        eprintln!("{} applied \u{2192} {}", report.label(), outputs.join(", "));
                    }
                    PassOutcome::Skipped { reason } => {
                        eprintln!("{} skipped \u{2014} {reason}", report.label());
                    }
                }
            }
            write_out(&passed, &dest)?;
        }
        PassMode::Skip => write_out(&file, &dest)?,
        PassMode::Strip => {
            if is_jsonl_ext_path(&dest) || is_jsonl_path(&dest) {
                return Err(TelemetryError::Unsupported(
                    "--strip-passes writes .telemetry output only".into(),
                ));
            }
            if dest == src {
                // In-place strip: write next to the destination, then swap.
                let dir = dest.parent().unwrap_or_else(|| Path::new("."));
                let name = dest
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("recording");
                let temp = dir.join(format!(".{name}.strip-tmp"));
                write_from_source_stripped(&file, &temp)?;
                drop(file);
                std::fs::rename(&temp, &dest)
                    .map_err(telemetry_format::TelemetryFormatError::Io)
                    .map_err(TelemetryError::Telemetry)?;
            } else {
                write_from_source_stripped(&file, &dest)?;
            }
        }
    }
    println!("{}", dest.display());
    Ok(())
}
