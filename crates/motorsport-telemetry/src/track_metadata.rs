//! Bounded, opt-out discovery of Omatrack-compatible `TRACK.yml` metadata.

mod yaml;

use globset::GlobBuilder;
use motorsport_telemetry_core::MetadataMap;
use serde_json::Value;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use thiserror::Error;

const MAX_BYTES: u64 = 1_048_576;

/// Controls filesystem metadata discovery when opening a recording.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct OpenOptions {
    /// Optional ancestor directory, inclusive, for parent `TRACK.yml` traversal.
    /// Without it only the recording's own directory is examined. Both paths
    /// are canonicalized; the root must be a directory containing the file.
    pub root_path: Option<PathBuf>,
    /// Ignore all external `TRACK.yml` files. Embedded metadata remains intact.
    /// An explicitly supplied root is still validated.
    pub ignore_track_yml: bool,
}

/// A metadata file or discovery boundary could not be used safely.
#[derive(Debug, Error)]
pub enum TrackMetadataError {
    /// A recording, root, or metadata path could not be accessed.
    #[error("{}: {source}", path.display())]
    Io {
        /// The failing path.
        path: PathBuf,
        /// The underlying filesystem error.
        #[source]
        source: io::Error,
    },
    /// The root does not contain the recording, or a metadata symlink escapes.
    #[error("{}: {message}", path.display())]
    Boundary {
        /// The path rejected by the boundary check.
        path: PathBuf,
        /// The failed requirement.
        message: String,
    },
    /// YAML, a matcher, or a recognized metadata field is malformed.
    #[error("{}: {message}", path.display())]
    Invalid {
        /// The `TRACK.yml` that failed validation.
        path: PathBuf,
        /// The parser or validation error (including the rule index).
        message: String,
    },
}

fn io_error(path: &Path, source: io::Error) -> TrackMetadataError {
    TrackMetadataError::Io {
        path: path.to_owned(),
        source,
    }
}
fn invalid(path: &Path, message: impl Into<String>) -> TrackMetadataError {
    TrackMetadataError::Invalid {
        path: path.to_owned(),
        message: message.into(),
    }
}
fn boundary(path: &Path, message: impl Into<String>) -> TrackMetadataError {
    TrackMetadataError::Boundary {
        path: path.to_owned(),
        message: message.into(),
    }
}

/// Return layers rather than flattening them: a parent null must mask embedded
/// metadata before a child begins adding new fields to that map.
pub(crate) fn load(
    path: &Path,
    options: &OpenOptions,
) -> Result<Vec<MetadataMap>, TrackMetadataError> {
    if options.ignore_track_yml && options.root_path.is_none() {
        return Ok(Vec::new());
    }
    let file = fs::canonicalize(path).map_err(|e| io_error(path, e))?;
    let directory = file
        .parent()
        .ok_or_else(|| boundary(path, "recording has no parent directory"))?;
    let root = match &options.root_path {
        Some(root) => {
            let canonical = fs::canonicalize(root).map_err(|e| io_error(root, e))?;
            if !canonical.is_dir() || !directory.starts_with(&canonical) {
                return Err(boundary(
                    root,
                    format!(
                        "root_path must be an ancestor directory of {}",
                        file.display()
                    ),
                ));
            }
            canonical
        }
        None => directory.to_owned(),
    };
    if options.ignore_track_yml {
        return Ok(Vec::new());
    }
    let directories: Vec<_> = directory
        .ancestors()
        .take_while(|p| p.starts_with(&root))
        .collect();
    let mut layers = Vec::new();
    for directory in directories.into_iter().rev() {
        let yaml = directory.join("TRACK.yml");
        match fs::symlink_metadata(&yaml) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(io_error(&yaml, error)),
            Ok(_) => {}
        }
        let target = fs::canonicalize(&yaml).map_err(|e| io_error(&yaml, e))?;
        if !target.starts_with(&root) {
            return Err(boundary(
                &yaml,
                format!(
                    "TRACK.yml resolves outside metadata root {}",
                    root.display()
                ),
            ));
        }
        if !target.is_file() {
            return Err(invalid(&yaml, "TRACK.yml must be a regular file"));
        }
        let input = fs::File::open(&target).map_err(|e| io_error(&yaml, e))?;
        let mut bytes = Vec::new();
        input
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| io_error(&yaml, e))?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid(&yaml, "TRACK.yml exceeds 1 MiB"));
        }
        let value = yaml::parse(&bytes).map_err(|e| invalid(&yaml, e))?;
        let mut document = match value {
            Value::Null => MetadataMap::new(),
            Value::Object(map) => map,
            _ => return Err(invalid(&yaml, "document must be a mapping")),
        };
        // Legacy Omatrack files emit this; it is not a metadata field.
        document.remove("schema");
        let overrides = document.remove("overrides");
        validate_metadata(&document, &yaml)?;
        layers.push(document);
        let relative = file
            .strip_prefix(directory)
            .map_err(|e| invalid(&yaml, e.to_string()))?;
        let rules = match overrides {
            None => continue,
            Some(Value::Array(rules)) => rules,
            _ => return Err(invalid(&yaml, "overrides must be a sequence")),
        };
        for (index, rule) in rules.into_iter().enumerate() {
            let rule_name = format!("overrides[{}]", index + 1);
            let mut rule = rule
                .as_object()
                .cloned()
                .ok_or_else(|| invalid(&yaml, format!("{rule_name} must be a mapping")))?;
            let pattern = rule
                .remove("match")
                .ok_or_else(|| invalid(&yaml, format!("{rule_name} requires match")))?;
            let mut metadata = rule
                .remove("metadata")
                .and_then(|v| v.as_object().cloned())
                .ok_or_else(|| {
                    invalid(&yaml, format!("{rule_name} requires a metadata mapping"))
                })?;
            if !rule.is_empty() {
                return Err(invalid(
                    &yaml,
                    format!("{rule_name} only accepts match and metadata"),
                ));
            }
            metadata.remove("schema");
            if metadata.contains_key("overrides") {
                return Err(invalid(
                    &yaml,
                    format!("{rule_name} cannot contain nested overrides"),
                ));
            }
            validate_metadata(&metadata, &yaml)?;
            if matches_path(&pattern, relative, &yaml, &rule_name)? {
                layers.push(metadata);
            }
        }
    }
    Ok(layers)
}

fn matches_path(
    pattern: &Value,
    relative: &Path,
    yaml: &Path,
    rule: &str,
) -> Result<bool, TrackMetadataError> {
    let patterns: Vec<&Value> = match pattern {
        Value::String(_) => vec![pattern],
        Value::Array(values) if !values.is_empty() => values.iter().collect(),
        _ => {
            return Err(invalid(
                yaml,
                format!("{rule}.match must be a glob or nonempty list of globs"),
            ))
        }
    };
    let mut matched = false;
    for pattern in patterns {
        let pattern = pattern
            .as_str()
            .ok_or_else(|| invalid(yaml, format!("{rule}.match must contain only strings")))?;
        if pattern.is_empty()
            || pattern.starts_with('/')
            || pattern.contains(['\\', ':'])
            || pattern.split('/').any(|part| matches!(part, "." | ".."))
        {
            return Err(invalid(
                yaml,
                format!("{rule}: glob must be a relative /-separated path: {pattern:?}"),
            ));
        }
        let glob = GlobBuilder::new(pattern)
            .literal_separator(true)
            .backslash_escape(false)
            .build()
            .map_err(|e| invalid(yaml, format!("{rule}: {e}")))?;
        // Validate every pattern, even after one matched.
        matched |= glob.compile_matcher().is_match(relative);
    }
    Ok(matched)
}

fn validate_metadata(map: &MetadataMap, path: &Path) -> Result<(), TrackMetadataError> {
    let text = |name: &str, value: &Value| {
        if value.is_string() || value.is_number() || value.is_null() {
            Ok(())
        } else {
            Err(invalid(
                path,
                format!("{name} must be text, a number, or null"),
            ))
        }
    };
    for key in ["event", "session", "date", "time"] {
        if let Some(value) = map.get(key) {
            text(key, value)?;
        }
    }
    for key in ["driver", "car", "track"] {
        if let Some(value) = map.get(key) {
            if value.is_null() {
                continue;
            }
            let fields = value
                .as_object()
                .ok_or_else(|| invalid(path, format!("{key} must be a mapping or null")))?;
            if let Some(name) = fields.get("name") {
                text(&format!("{key}.name"), name)?;
            }
        }
    }
    if let Some(mappings) = map.get("driver").and_then(|d| d.get("mappings")) {
        if !mappings.is_null() {
            let mappings = mappings
                .as_object()
                .ok_or_else(|| invalid(path, "driver.mappings must be a mapping or null"))?;
            for (id, name) in mappings {
                text(&format!("driver.mappings.{id}"), name)?;
            }
        }
    }
    Ok(())
}
