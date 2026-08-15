use motorsport_telemetry::{open, TelemetryError};
use std::path::{Path, PathBuf};
use telemetry_format::{
    is_jsonl_ext_path, is_jsonl_path, write_from_source, write_jsonl_extension_from_source,
    write_jsonl_from_source,
};

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
    if is_jsonl_ext_path(&dest) {
        write_jsonl_extension_from_source(&file, &dest)?;
    } else if is_jsonl_path(&dest) {
        write_jsonl_from_source(&file, &dest)?;
    } else {
        write_from_source(&file, &dest)?;
    }
    println!("{}", dest.display());
    Ok(())
}
