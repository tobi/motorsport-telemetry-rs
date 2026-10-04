//! Default compressed writer and a view that removes processing-pass outputs.

use motorsport_telemetry_core::TelemetrySource;
use std::collections::HashSet;
use std::path::Path;
use thiserror::Error;

/// Errors raised while reading or writing a telemetry document.
#[derive(Debug, Error)]
pub enum TelemetryFormatError {
    /// A structural/content problem with the source or document.
    #[error("{0}")]
    Invalid(String),
    /// An underlying I/O failure.
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Writes a `.telemetry` recording as zstd-compressed MTJ JSONL.
pub fn write_telemetry(
    source: &dyn TelemetrySource,
    dest: impl AsRef<Path>,
) -> Result<(), TelemetryFormatError> {
    crate::write_jsonl_from_source(source, dest)
}

/// Writes a compressed recording with applied-pass outputs removed.
///
/// Preserves the original source identity and clears pass provenance.
pub fn write_telemetry_stripped(
    source: &dyn TelemetrySource,
    dest: impl AsRef<Path>,
) -> Result<(), TelemetryFormatError> {
    write_telemetry(&stripped_view(source), dest)
}

/// A view with every applied-pass output channel removed and the pass list cleared.
pub fn stripped_view(source: &dyn TelemetrySource) -> motorsport_telemetry_core::ViewSource<'_> {
    let outputs: HashSet<&str> = source
        .applied_passes()
        .iter()
        .flat_map(|pass| pass.outputs.iter().map(String::as_str))
        .collect();
    let mut view = motorsport_telemetry_core::ViewSource::new(source);
    view.retain(|_, channel| !outputs.contains(channel.name.as_str()));
    view.passes_mut().clear();
    view
}
