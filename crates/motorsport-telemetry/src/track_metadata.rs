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

/// Ordered external metadata, kept separate from embedded recording metadata.
///
/// Apply layers in order, rather than flattening them first: an ancestor null
/// must mask embedded fields before a nearer object adds new fields.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrackMetadataLayers {
    /// Root-to-leaf defaults, followed by matching rules from each file.
    /// Directory resolution includes defaults only. Control fields are removed.
    pub layers: Vec<MetadataMap>,
    /// One discovered `TRACK.yml` path per document read, in root-to-leaf order.
    /// Paths use canonical directories but retain a metadata symlink's name.
    /// Empty documents are included; missing documents are omitted.
    pub paths: Vec<PathBuf>,
}

impl TrackMetadataLayers {
    /// Apply each ordered layer to embedded or otherwise inherited metadata.
    pub fn apply_to(&self, metadata: &mut MetadataMap) {
        for layer in &self.layers {
            motorsport_telemetry_core::merge_metadata(metadata, layer);
        }
    }
}

/// Resolve external metadata for any existing regular recording file.
///
/// The recording's contents and format are never read or decoded; empty
/// discovery stubs, MOV and MKV videos are accepted. Canonical paths determine
/// containment and matcher paths. The target and any explicit root are checked
/// even when external metadata is disabled.
pub fn load_track_metadata(
    path: impl AsRef<Path>,
    options: &OpenOptions,
) -> Result<TrackMetadataLayers, TrackMetadataError> {
    let path = path.as_ref();
    let file = fs::canonicalize(path).map_err(|e| io_error(path, e))?;
    if !file.is_file() {
        return Err(invalid(path, "recording must be a regular file"));
    }
    let directory = file
        .parent()
        .ok_or_else(|| boundary(path, "recording has no parent directory"))?;
    let root = metadata_root(directory, options.root_path.as_deref())?;
    if options.ignore_track_yml {
        return Ok(TrackMetadataLayers::default());
    }
    resolve(directory, &root, Some(&file), false)
}

/// Resolve defaults for an existing directory, without a fictitious recording.
///
/// All rules in documents read are validated, but neither applied nor inherited
/// as metadata. `exclude_target` skips the target directory's own `TRACK.yml`,
/// for a folder editor's inherited defaults. Without an explicit ancestor root,
/// excluding the target yields no layers. Canonical target/root checks still
/// apply when metadata is disabled or the target is excluded.
pub fn load_track_directory_metadata(
    path: impl AsRef<Path>,
    options: &OpenOptions,
    exclude_target: bool,
) -> Result<TrackMetadataLayers, TrackMetadataError> {
    let path = path.as_ref();
    let directory = fs::canonicalize(path).map_err(|e| io_error(path, e))?;
    if !directory.is_dir() {
        return Err(boundary(path, "metadata target must be a directory"));
    }
    let root = metadata_root(&directory, options.root_path.as_deref())?;
    if options.ignore_track_yml {
        return Ok(TrackMetadataLayers::default());
    }
    resolve(&directory, &root, None, exclude_target)
}

/// Read and validate a single bounded JSON-compatible metadata document.
///
/// Preserves `schema`, `overrides`, and unknown keys for an editor to serialize;
/// does not resolve layers, traverse ancestors, or write anything. Empty/null
/// documents return an empty map. Every rule is validated with the same policy
/// as recording resolution. An explicit root must be an existing directory and
/// contain the canonical document. Without it, the document's containing
/// directory is the boundary; metadata symlinks cannot escape that boundary.
pub fn read_track_metadata_document(
    path: impl AsRef<Path>,
    root_path: Option<&Path>,
) -> Result<MetadataMap, TrackMetadataError> {
    let path = path.as_ref();
    let root = if let Some(root) = root_path {
        canonical_root(root)?
    } else {
        let parent = path
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        fs::canonicalize(parent).map_err(|e| io_error(parent, e))?
    };
    let document = read_document(path, &root)?;
    document_layers(&document, None, path)?;
    Ok(document)
}

/// Existing facade opens retain their opt-out behavior before decoder opening.
pub(crate) fn load(
    path: &Path,
    options: &OpenOptions,
) -> Result<Vec<MetadataMap>, TrackMetadataError> {
    if options.ignore_track_yml && options.root_path.is_none() {
        return Ok(Vec::new());
    }
    Ok(load_track_metadata(path, options)?.layers)
}

fn canonical_root(root: &Path) -> Result<PathBuf, TrackMetadataError> {
    let canonical = fs::canonicalize(root).map_err(|e| io_error(root, e))?;
    if !canonical.is_dir() {
        return Err(boundary(root, "metadata root must be a directory"));
    }
    Ok(canonical)
}

fn metadata_root(
    directory: &Path,
    root_path: Option<&Path>,
) -> Result<PathBuf, TrackMetadataError> {
    let Some(root) = root_path else {
        return Ok(directory.to_owned());
    };
    let canonical = canonical_root(root)?;
    if !directory.starts_with(&canonical) {
        return Err(boundary(
            root,
            format!(
                "root_path must be an ancestor directory of {}",
                directory.display()
            ),
        ));
    }
    Ok(canonical)
}

fn resolve(
    directory: &Path,
    root: &Path,
    recording: Option<&Path>,
    exclude_target: bool,
) -> Result<TrackMetadataLayers, TrackMetadataError> {
    let directories: Vec<_> = directory
        .ancestors()
        .take_while(|p| p.starts_with(root))
        .filter(|p| !exclude_target || *p != directory)
        .collect();
    let mut result = TrackMetadataLayers::default();
    for directory in directories.into_iter().rev() {
        let yaml = directory.join("TRACK.yml");
        match fs::symlink_metadata(&yaml) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(io_error(&yaml, error)),
            Ok(_) => {}
        }
        let document = read_document(&yaml, root)?;
        let relative = recording
            .map(|file| file.strip_prefix(directory))
            .transpose()
            .map_err(|e| invalid(&yaml, e.to_string()))?;
        result
            .layers
            .extend(document_layers(&document, relative, &yaml)?);
        result.paths.push(yaml);
    }
    Ok(result)
}

fn read_document(path: &Path, root: &Path) -> Result<MetadataMap, TrackMetadataError> {
    let target = fs::canonicalize(path).map_err(|e| io_error(path, e))?;
    if !target.starts_with(root) {
        return Err(boundary(
            path,
            format!(
                "TRACK.yml resolves outside metadata root {}",
                root.display()
            ),
        ));
    }
    if !target.is_file() {
        return Err(invalid(path, "TRACK.yml must be a regular file"));
    }
    let input = fs::File::open(&target).map_err(|e| io_error(path, e))?;
    let mut bytes = Vec::new();
    input
        .take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| io_error(path, e))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(invalid(path, "TRACK.yml exceeds 1 MiB"));
    }
    match yaml::parse(&bytes).map_err(|e| invalid(path, e))? {
        Value::Null => Ok(MetadataMap::new()),
        Value::Object(map) => Ok(map),
        _ => Err(invalid(path, "document must be a mapping")),
    }
}

fn document_layers(
    document: &MetadataMap,
    relative: Option<&Path>,
    yaml: &Path,
) -> Result<Vec<MetadataMap>, TrackMetadataError> {
    let mut defaults = document.clone();
    // Legacy schema is accepted but never projected onto effective metadata.
    defaults.remove("schema");
    let overrides = defaults.remove("overrides");
    validate_metadata(&defaults, yaml)?;
    let mut layers = vec![defaults];
    let rules = match overrides {
        None => return Ok(layers),
        Some(Value::Array(rules)) => rules,
        _ => return Err(invalid(yaml, "overrides must be a sequence")),
    };
    for (index, rule) in rules.into_iter().enumerate() {
        let rule_name = format!("overrides[{}]", index + 1);
        let mut rule = rule
            .as_object()
            .cloned()
            .ok_or_else(|| invalid(yaml, format!("{rule_name} must be a mapping")))?;
        let pattern = rule
            .remove("match")
            .ok_or_else(|| invalid(yaml, format!("{rule_name} requires match")))?;
        let mut metadata = rule
            .remove("metadata")
            .and_then(|v| v.as_object().cloned())
            .ok_or_else(|| invalid(yaml, format!("{rule_name} requires a metadata mapping")))?;
        if !rule.is_empty() {
            return Err(invalid(
                yaml,
                format!("{rule_name} only accepts match and metadata"),
            ));
        }
        metadata.remove("schema");
        if metadata.contains_key("overrides") {
            return Err(invalid(
                yaml,
                format!("{rule_name} cannot contain nested overrides"),
            ));
        }
        validate_metadata(&metadata, yaml)?;
        if matches_path(&pattern, relative, yaml, &rule_name)? {
            layers.push(metadata);
        }
    }
    Ok(layers)
}

fn matches_path(
    pattern: &Value,
    relative: Option<&Path>,
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
        let matcher = glob.compile_matcher();
        matched |= relative.is_some_and(|path| matcher.is_match(path));
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
