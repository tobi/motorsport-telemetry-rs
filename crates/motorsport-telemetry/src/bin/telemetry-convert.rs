use motorsport_telemetry::{open, TelemetryError};
use std::path::{Path, PathBuf};
use telemetry_format::write_from_source;

fn default_dest(src: &Path) -> PathBuf {
    let mut dest = src.to_path_buf();
    let name = src
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("recording");
    dest.set_file_name(format!("{name}.telemetry"));
    dest
}

fn main() -> Result<(), TelemetryError> {
    let src = std::env::args_os().nth(1).ok_or_else(|| {
        TelemetryError::Unsupported("usage: telemetry-convert INPUT [OUTPUT]".into())
    })?;
    let src = PathBuf::from(src);
    let dest = std::env::args_os()
        .nth(2)
        .map(PathBuf::from)
        .unwrap_or_else(|| default_dest(&src));
    let file = open(&src)?;
    write_from_source(&file, &dest)?;
    println!("{}", dest.display());
    Ok(())
}
