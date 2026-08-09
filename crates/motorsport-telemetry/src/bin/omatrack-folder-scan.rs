use blake3::Hasher;
use memmap2::MmapOptions;
use motorsport_telemetry::motorsport_telemetry_core::TelemetrySource;
use motorsport_telemetry::{open_metadata, SignalRoles};
use serde::{Deserialize, Serialize};
use serde_yaml::Value as YamlValue;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::{self, BufReader, BufWriter, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

const CACHE_VERSION: u32 = 4;
const SUMMARY_VERSION: u32 = 10;
const CACHE_NAMESPACE: &[u8] = b"omatrack-session-index-blake3-v1\0";
const FINGERPRINT_LIMIT: u64 = 1024 * 1024;
const MAX_CACHE_AGE_MS: u64 = 90 * 24 * 60 * 60 * 1000;
const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mov", "mkv", "avi", "m4v", "webm"];
const CANDIDATE_EXTENSIONS: &[&str] = &[
    "pds", "ld", "ldx", "vbo", "mp4", "mov", "mkv", "avi", "m4v", "webm",
];

const EXPECTED_ROOT_RAW: [usize; 7] = [31, 1_033, 54, 14, 4, 8, 4];
const EXPECTED_ROOT_ROWS: [usize; 7] = [31, 367, 54, 14, 4, 8, 4];
const EXPECTED_RAW: usize = 1_148;
const EXPECTED_ROWS: usize = 482;
const EXPECTED_UNIQUE: usize = 456;
const EXPECTED_ORPHAN_LDX: usize = 300;
const EXPECTED_LDX_COMPANIONS: usize = 366;
const EXPECTED_SOURCE_BYTES: u64 = 171_908_976_503;
const EXPECTED_FINGERPRINT_BYTES: u64 = 476_820_927;

#[derive(Debug)]
struct Arguments {
    config: PathBuf,
    run_dir: PathBuf,
    repetitions: usize,
    check_inventory: bool,
}

#[derive(Debug, Deserialize)]
struct OmatrackConfig {
    telemetry_dirs: Vec<PathBuf>,
}

#[derive(Debug, Clone, Serialize)]
struct FileRow {
    name: String,
    path: PathBuf,
    modified_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
struct FolderNode {
    name: String,
    path: PathBuf,
    modified_ms: u64,
    folders: Vec<FolderNode>,
    files: Vec<FileRow>,
}

#[derive(Debug)]
struct FolderBuilder {
    name: String,
    path: PathBuf,
    modified_ms: u64,
    folders: HashMap<String, FolderBuilder>,
    files: Vec<FileRow>,
}

#[derive(Debug, Serialize)]
struct RootManifest {
    root: PathBuf,
    available: bool,
    raw_matches: usize,
    source_rows: usize,
    tree: FolderNode,
}

#[derive(Debug, Serialize)]
struct Manifest {
    roots: Vec<RootManifest>,
    track_metadata_paths: Vec<PathBuf>,
    raw_matches: usize,
    source_rows: usize,
    unique_sources: Vec<PathBuf>,
    unique_source_bytes: u64,
    orphan_ldx: usize,
    ldx_companions: usize,
    extensions: BTreeMap<String, usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct ChannelSummary {
    name: String,
    unit: String,
    frequency_hz: f64,
    examples: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CachedSummary {
    version: u32,
    format: String,
    channel_count: usize,
    sampled_channel_count: usize,
    sample_count: u64,
    duration_ns: u64,
    driver_ids: Vec<i64>,
    lap_count: usize,
    complete_lap_count: usize,
    fastest_lap_ns: Option<u64>,
    driver: String,
    vehicle: String,
    venue: String,
    event: String,
    session: String,
    date: String,
    time: String,
    source_channels: Vec<ChannelSummary>,
    automatic_channel_mappings: BTreeMap<String, String>,
    gps_latitude: Option<f64>,
    gps_longitude: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CacheEntry {
    path: String,
    supported: bool,
    last_seen: u64,
    metadata: Option<CachedSummary>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct CacheFile {
    version: u32,
    entries: BTreeMap<String, CacheEntry>,
}

#[derive(Debug, Clone, Default, Serialize)]
struct PhaseTimes {
    discovery_ms: f64,
    folder_metadata_ms: f64,
    fingerprint_ms: f64,
    summary_parse_ms: f64,
    cache_serialization_ms: f64,
    total_ms: f64,
}

#[derive(Debug, Clone, Serialize)]
struct ScanResult {
    phases: PhaseTimes,
    raw_matches: usize,
    source_rows: usize,
    unique_sources: usize,
    unique_source_bytes: u64,
    fingerprint_bytes: u64,
    sessions: usize,
    unsupported: usize,
    errors: usize,
    cache_hits: usize,
    cache_misses: usize,
    track_metadata_files: usize,
}

#[derive(Debug, Serialize)]
struct RepetitionResult {
    repetition: usize,
    cold: ScanResult,
    warm: ScanResult,
}

fn main() {
    if let Err(error) = run() {
        eprintln!("omatrack-folder-scan: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let arguments = parse_arguments()?;
    require_tmp_directory(&arguments.run_dir)?;
    fs::create_dir_all(&arguments.run_dir).map_err(display_error("create run directory"))?;
    let roots = read_config(&arguments.config)?;

    let validation = discover(&roots).map_err(|error| error.to_string())?;
    validate_inventory(&validation, arguments.check_inventory)?;
    write_json(&arguments.run_dir.join("manifest.json"), &validation)?;

    let cache_path = arguments.run_dir.join("session-index.json");
    remove_benchmark_cache(&cache_path)?;
    let priming = scan(&roots, &cache_path, arguments.check_inventory)?;
    if priming.cache_hits != 0 || priming.cache_misses != priming.unique_sources {
        return Err("untimed priming scan did not start with an empty metadata cache".into());
    }
    remove_benchmark_cache(&cache_path)?;
    println!("PRIME validated_sources={}", priming.unique_sources);

    let mut repetitions = Vec::with_capacity(arguments.repetitions);
    for repetition in 1..=arguments.repetitions {
        remove_benchmark_cache(&cache_path)?;
        let cold = scan(&roots, &cache_path, arguments.check_inventory)?;
        let warm = scan(&roots, &cache_path, arguments.check_inventory)?;
        validate_pair(&cold, &warm)?;
        println!(
            "RUN repetition={repetition} cold_ms={:.3} warm_ms={:.3} discovery_ms={:.3} metadata_ms={:.3} fingerprint_ms={:.3} summary_ms={:.3} serialization_ms={:.3}",
            cold.phases.total_ms,
            warm.phases.total_ms,
            cold.phases.discovery_ms,
            cold.phases.folder_metadata_ms,
            cold.phases.fingerprint_ms,
            cold.phases.summary_parse_ms,
            cold.phases.cache_serialization_ms,
        );
        repetitions.push(RepetitionResult {
            repetition,
            cold,
            warm,
        });
    }
    write_json(&arguments.run_dir.join("report.json"), &repetitions)?;
    print_metrics(&repetitions);
    Ok(())
}

fn default_config() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"))
        .join("omatrack/omatrack.yml")
}

fn parse_arguments() -> Result<Arguments, String> {
    let mut config = default_config();
    let mut run_dir = None;
    let mut repetitions = 3usize;
    let mut check_inventory = true;
    let mut args = std::env::args_os().skip(1);
    while let Some(argument) = args.next() {
        match argument.to_string_lossy().as_ref() {
            "--config" => config = PathBuf::from(args.next().ok_or("--config needs a path")?),
            "--run-dir" => {
                run_dir = Some(PathBuf::from(args.next().ok_or("--run-dir needs a path")?))
            }
            "--repetitions" => {
                repetitions = args
                    .next()
                    .ok_or("--repetitions needs a number")?
                    .to_string_lossy()
                    .parse()
                    .map_err(|_| "invalid --repetitions")?;
                if repetitions == 0 {
                    return Err("--repetitions must be at least one".into());
                }
            }
            "--skip-inventory-check" => check_inventory = false,
            "-h" | "--help" => {
                println!("Usage: omatrack-folder-scan --run-dir /tmp/DIR [--config PATH] [--repetitions N] [--skip-inventory-check]");
                std::process::exit(0);
            }
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(Arguments {
        config,
        run_dir: run_dir.ok_or("--run-dir is required")?,
        repetitions,
        check_inventory,
    })
}

fn require_tmp_directory(path: &Path) -> Result<(), String> {
    if !path.is_absolute() || !path.starts_with("/tmp") || path == Path::new("/tmp") {
        return Err(format!(
            "run directory must be a child of /tmp: {}",
            path.display()
        ));
    }
    Ok(())
}

fn read_config(path: &Path) -> Result<Vec<PathBuf>, String> {
    let file = File::open(path).map_err(display_error("open Omatrack config"))?;
    let config: OmatrackConfig = serde_yaml::from_reader(BufReader::new(file))
        .map_err(|error| format!("parse {}: {error}", path.display()))?;
    if config.telemetry_dirs.is_empty() {
        return Err(format!("{} has no telemetry_dirs", path.display()));
    }
    Ok(config.telemetry_dirs)
}

fn discover(roots: &[PathBuf]) -> io::Result<Manifest> {
    let mut unique_sources = BTreeSet::new();
    let mut metadata_paths = BTreeSet::new();
    let mut root_manifests = Vec::with_capacity(roots.len());
    let mut total_raw = 0usize;
    let mut total_rows = 0usize;
    let mut total_orphans = 0usize;
    let mut total_companions = 0usize;

    for root in roots {
        let mut candidates = Vec::new();
        if root.is_dir() {
            walk_candidates(root, &mut candidates)?;
        }
        let raw_matches = candidates.len();
        total_raw += raw_matches;
        let mut source_paths = BTreeSet::new();
        for path in candidates {
            if path.file_name().is_some_and(|name| name == "TRACK.yml") {
                metadata_paths.insert(absolute_fallback(&path)?);
                continue;
            }
            match telemetry_path_for_input(&path) {
                Some((resolved, ldx_companion)) => {
                    total_companions += usize::from(ldx_companion);
                    source_paths.insert(canonical_fallback(&resolved)?);
                }
                None => total_orphans += 1,
            }
        }
        total_rows += source_paths.len();
        unique_sources.extend(source_paths.iter().cloned());
        let tree = build_file_tree(root, &source_paths)?;
        root_manifests.push(RootManifest {
            root: root.clone(),
            available: root.is_dir(),
            raw_matches,
            source_rows: source_paths.len(),
            tree,
        });
    }

    let mut extensions = BTreeMap::new();
    let mut source_bytes = 0u64;
    for path in &unique_sources {
        *extensions.entry(extension(path)).or_insert(0) += 1;
        source_bytes = source_bytes.saturating_add(fs::metadata(path)?.len());
    }
    Ok(Manifest {
        roots: root_manifests,
        track_metadata_paths: metadata_paths.into_iter().collect(),
        raw_matches: total_raw,
        source_rows: total_rows,
        unique_sources: unique_sources.into_iter().collect(),
        unique_source_bytes: source_bytes,
        orphan_ldx: total_orphans,
        ldx_companions: total_companions,
        extensions,
    })
}

fn walk_candidates(directory: &Path, output: &mut Vec<PathBuf>) -> io::Result<()> {
    let mut pending = vec![directory.to_path_buf()];
    while let Some(current) = pending.pop() {
        for entry in fs::read_dir(current)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            if file_type.is_dir() {
                pending.push(entry.path());
            } else if file_type.is_file() && is_candidate(&entry.path()) {
                output.push(entry.path());
            }
        }
    }
    Ok(())
}

fn is_candidate(path: &Path) -> bool {
    path.file_name().is_some_and(|name| name == "TRACK.yml")
        || CANDIDATE_EXTENSIONS.contains(&extension(path).as_str())
}

fn extension(path: &Path) -> String {
    path.extension()
        .and_then(|value| value.to_str())
        .unwrap_or("")
        .to_ascii_lowercase()
}

fn telemetry_path_for_input(path: &Path) -> Option<(PathBuf, bool)> {
    if extension(path) != "ldx" {
        return Some((path.to_path_buf(), false));
    }
    for candidate in ["ld", "LD"] {
        let companion = path.with_extension(candidate);
        if companion.exists() {
            return Some((companion, true));
        }
    }
    None
}

fn absolute_fallback(path: &Path) -> io::Result<PathBuf> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        Ok(std::env::current_dir()?.join(path))
    }
}

fn canonical_fallback(path: &Path) -> io::Result<PathBuf> {
    fs::canonicalize(path).or_else(|_| absolute_fallback(path))
}

impl FolderBuilder {
    fn new(name: String, path: PathBuf) -> Self {
        Self {
            name,
            path,
            modified_ms: 0,
            folders: HashMap::new(),
            files: Vec::new(),
        }
    }

    fn insert(&mut self, root: &Path, path: &Path) -> io::Result<()> {
        let metadata = fs::metadata(path)?;
        let modified_ms = system_time_ms(metadata.modified().unwrap_or(UNIX_EPOCH));
        self.modified_ms = self.modified_ms.max(modified_ms);
        let components = path
            .strip_prefix(root)
            .ok()
            .filter(|relative| !relative.as_os_str().is_empty())
            .map(|relative| {
                relative
                    .components()
                    .filter_map(|component| match component {
                        Component::Normal(value) => Some(value.to_string_lossy().into_owned()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            })
            .filter(|parts| !parts.is_empty())
            .unwrap_or_else(|| {
                vec![path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
                    .into_owned()]
            });
        let mut folder = self;
        for component in &components[..components.len() - 1] {
            let child_path = folder.path.join(component);
            folder = folder
                .folders
                .entry(component.clone())
                .or_insert_with(|| FolderBuilder::new(component.clone(), child_path));
            folder.modified_ms = folder.modified_ms.max(modified_ms);
        }
        folder.files.push(FileRow {
            name: components.last().cloned().unwrap_or_default(),
            path: path.to_path_buf(),
            modified_ms,
        });
        Ok(())
    }

    fn finish(self) -> FolderNode {
        let mut folders = self
            .folders
            .into_values()
            .map(FolderBuilder::finish)
            .collect::<Vec<_>>();
        let mut files = self.files;
        folders.sort_by(newest_then_name_folder);
        files.sort_by(newest_then_name_file);
        FolderNode {
            name: self.name,
            path: self.path,
            modified_ms: self.modified_ms,
            folders,
            files,
        }
    }
}

fn newest_then_name_folder(left: &FolderNode, right: &FolderNode) -> std::cmp::Ordering {
    right.modified_ms.cmp(&left.modified_ms).then_with(|| {
        left.name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
    })
}

fn newest_then_name_file(left: &FileRow, right: &FileRow) -> std::cmp::Ordering {
    right.modified_ms.cmp(&left.modified_ms).then_with(|| {
        left.name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
    })
}

fn build_file_tree(root: &Path, sources: &BTreeSet<PathBuf>) -> io::Result<FolderNode> {
    let absolute_root = absolute_fallback(root)?;
    let name = absolute_root
        .file_name()
        .filter(|name| !name.is_empty())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| absolute_root.display().to_string());
    let mut builder = FolderBuilder::new(name, absolute_root.clone());
    for source in sources {
        builder.insert(&absolute_root, source)?;
    }
    Ok(builder.finish())
}

fn scan(roots: &[PathBuf], cache_path: &Path, check_inventory: bool) -> Result<ScanResult, String> {
    let total_start = Instant::now();
    let discovery_start = Instant::now();
    let manifest = discover(roots).map_err(|error| error.to_string())?;
    let discovery_ms = elapsed_ms(discovery_start);
    validate_inventory(&manifest, check_inventory)?;

    let metadata_start = Instant::now();
    let track_metadata_files = read_folder_metadata(&manifest.unique_sources)?;
    let folder_metadata_ms = elapsed_ms(metadata_start);
    let mut cache = load_cache(cache_path)?;
    let now = system_time_ms(SystemTime::now());
    let mut fingerprint_ms = 0.0;
    let mut summary_parse_ms = 0.0;
    let mut fingerprint_bytes = 0u64;
    let mut sessions = 0usize;
    let mut unsupported = 0usize;
    let mut errors = 0usize;
    let mut cache_hits = 0usize;
    let mut cache_misses = 0usize;
    for path in &manifest.unique_sources {
        let fingerprint_start = Instant::now();
        let (fingerprint, bytes) = fingerprint(path)
            .map_err(|error| format!("fingerprint {}: {error}", path.display()))?;
        fingerprint_ms += elapsed_ms(fingerprint_start);
        fingerprint_bytes += bytes;
        if let Some(entry) = cache.entries.get_mut(&fingerprint) {
            entry.last_seen = now;
            cache_hits += 1;
            if entry.supported {
                let summary = entry.metadata.as_ref().ok_or_else(|| {
                    format!(
                        "supported cache entry has no metadata for {}",
                        path.display()
                    )
                })?;
                if summary.version != SUMMARY_VERSION {
                    return Err(format!("wrong summary schema for {}", path.display()));
                }
                sessions += 1;
            } else {
                unsupported += 1;
            }
            continue;
        }

        cache_misses += 1;
        let summary_start = Instant::now();
        let parsed = summarize(path);
        summary_parse_ms += elapsed_ms(summary_start);
        let (supported, metadata) = match parsed {
            Ok(summary) => {
                sessions += 1;
                (true, Some(summary))
            }
            Err(_) => {
                unsupported += 1;
                errors += 1;
                (false, None)
            }
        };
        cache.entries.insert(
            fingerprint,
            CacheEntry {
                path: path.to_string_lossy().into_owned(),
                supported,
                last_seen: now,
                metadata,
            },
        );
    }

    let serialization_start = Instant::now();
    cache
        .entries
        .retain(|_, entry| now.saturating_sub(entry.last_seen) <= MAX_CACHE_AGE_MS);
    save_cache(cache_path, &cache)?;
    let cache_serialization_ms = elapsed_ms(serialization_start);
    let result = ScanResult {
        phases: PhaseTimes {
            discovery_ms,
            folder_metadata_ms,
            fingerprint_ms,
            summary_parse_ms,
            cache_serialization_ms,
            total_ms: elapsed_ms(total_start),
        },
        raw_matches: manifest.raw_matches,
        source_rows: manifest.source_rows,
        unique_sources: manifest.unique_sources.len(),
        unique_source_bytes: manifest.unique_source_bytes,
        fingerprint_bytes,
        sessions,
        unsupported,
        errors,
        cache_hits,
        cache_misses,
        track_metadata_files,
    };
    if check_inventory && result.fingerprint_bytes != EXPECTED_FINGERPRINT_BYTES {
        return Err(format!(
            "fingerprint bytes changed: expected {EXPECTED_FINGERPRINT_BYTES}, found {} (use --skip-inventory-check only after reviewing live inventory)",
            result.fingerprint_bytes
        ));
    }
    Ok(result)
}

fn fingerprint(path: &Path) -> io::Result<(String, u64)> {
    let canonical = canonical_fallback(path)?;
    let mut hash = Hasher::new();
    hash.update(CACHE_NAMESPACE);
    hash.update(b"primary\0");
    let mut bytes = add_file_fingerprint(&mut hash, &canonical)?;
    if extension(&canonical) == "ld" {
        hash.update(b"sidecar\0");
        let sidecar = canonical.with_extension("ldx");
        match add_file_fingerprint(&mut hash, &sidecar) {
            Ok(count) => bytes += count,
            Err(_) => {
                hash.update(b"missing\0");
            }
        }
    }
    Ok((hash.finalize().to_hex().to_string(), bytes))
}

fn add_file_fingerprint(hash: &mut Hasher, path: &Path) -> io::Result<u64> {
    let canonical = canonical_fallback(path)?;
    let metadata = fs::metadata(&canonical)?;
    let file = File::open(&canonical)?;
    hash.update(canonical.to_string_lossy().as_bytes());
    hash.update(b"\0");
    hash.update(metadata.len().to_string().as_bytes());
    hash.update(b"\0");
    let total = metadata.len().min(FINGERPRINT_LIMIT);
    if total > 0 {
        let mapping = unsafe { MmapOptions::new().len(total as usize).map(&file)? };
        hash.update(&mapping);
    }
    Ok(total)
}

fn summarize(path: &Path) -> Result<CachedSummary, String> {
    let is_video = VIDEO_EXTENSIONS.contains(&extension(path).as_str());
    let file = open_metadata(path).map_err(|error| error.to_string())?;
    let metadata = file.metadata();
    let (source_channels, automatic_channel_mappings, gps) = if is_video {
        (
            channel_summaries(&file),
            role_mappings(&file.signal_roles(), file.channels()),
            sampled_gps(&file),
        )
    } else {
        (Vec::new(), BTreeMap::new(), None)
    };
    Ok(CachedSummary {
        version: SUMMARY_VERSION,
        format: metadata.format,
        channel_count: metadata.channel_count,
        sampled_channel_count: metadata.sampled_channel_count,
        sample_count: metadata.sample_count,
        duration_ns: metadata.duration_ns,
        driver_ids: metadata.driver_ids,
        lap_count: metadata.laps.len(),
        complete_lap_count: metadata.laps.iter().filter(|lap| lap.complete).count(),
        fastest_lap_ns: metadata.fastest_lap.map(|lap| lap.duration_ns),
        driver: metadata.identity.driver,
        vehicle: metadata.identity.vehicle,
        venue: metadata.identity.venue,
        event: metadata.identity.event,
        session: metadata.identity.session,
        date: metadata.identity.date,
        time: metadata.identity.time,
        source_channels,
        automatic_channel_mappings,
        gps_latitude: gps.map(|pair| pair.0),
        gps_longitude: gps.map(|pair| pair.1),
    })
}

fn channel_summaries(file: &motorsport_telemetry::TelemetryFile) -> Vec<ChannelSummary> {
    let mut seen = BTreeSet::new();
    let mut output = Vec::new();
    for (channel_index, channel) in file.channels().iter().enumerate() {
        if channel.sample_count == 0 || channel.name.trim().is_empty() {
            continue;
        }
        if !seen.insert(channel.name.trim().to_lowercase()) {
            continue;
        }
        let mut examples = Vec::new();
        let mut example_set = BTreeSet::new();
        for slot in 0..9u64 {
            let index = if channel.sample_count == 1 {
                0
            } else {
                (channel.sample_count - 1) * slot / 8
            };
            if let Some(value) = decode_flat(file, channel_index, index) {
                if !value.is_finite() {
                    continue;
                }
                let formatted = format!("{value:.7}");
                if example_set.insert(formatted.clone()) {
                    examples.push(formatted);
                    if examples.len() == 5 {
                        break;
                    }
                }
            }
        }
        let frequency_hz = channel
            .chunks
            .first()
            .filter(|chunk| chunk.sample_period_ns > 0)
            .map(|chunk| 1e9 / chunk.sample_period_ns as f64)
            .unwrap_or(0.0);
        output.push(ChannelSummary {
            name: channel.name.trim().to_owned(),
            unit: channel.unit.trim().to_owned(),
            frequency_hz,
            examples,
        });
    }
    output
}

fn decode_flat(
    file: &motorsport_telemetry::TelemetryFile,
    channel_index: usize,
    mut index: u64,
) -> Option<f64> {
    for (chunk_index, chunk) in file.channels()[channel_index].chunks.iter().enumerate() {
        if index < chunk.sample_count {
            return Some(file.decode(channel_index, chunk_index, index));
        }
        index -= chunk.sample_count;
    }
    None
}

fn role_mappings(
    roles: &SignalRoles,
    channels: &[motorsport_telemetry::motorsport_telemetry_core::Channel],
) -> BTreeMap<String, String> {
    let mut output = BTreeMap::new();
    for (name, index) in [
        ("speed", roles.speed),
        ("throttle", roles.throttle),
        ("brake", roles.brake),
        ("lap_distance", roles.lap_distance),
        ("lap_number", roles.lap_number),
        ("gps_lat", roles.latitude),
        ("gps_lon", roles.longitude),
    ] {
        if let Some(index) = index {
            output.insert(name.to_owned(), channels[index].name.clone());
        }
    }
    output
}

fn sampled_gps(file: &motorsport_telemetry::TelemetryFile) -> Option<(f64, f64)> {
    let roles = file.signal_roles();
    let (latitude, longitude) = roles.latitude.zip(roles.longitude)?;
    let duration = file.channels()[latitude]
        .duration_ns
        .min(file.channels()[longitude].duration_ns);
    if duration == 0 {
        return None;
    }
    let mut latitudes = Vec::new();
    let mut longitudes = Vec::new();
    for sample in 1..=19u64 {
        let time = duration.saturating_mul(sample) / 20;
        let Some(lat) = file
            .sample_at(latitude, time, true)
            .and_then(|value| normalize_coordinate(value, &file.channels()[latitude].unit))
        else {
            continue;
        };
        let Some(lon) = file
            .sample_at(longitude, time, true)
            .and_then(|value| normalize_coordinate(value, &file.channels()[longitude].unit))
        else {
            continue;
        };
        if lat.is_finite()
            && lon.is_finite()
            && lat.abs() <= 90.0
            && lon.abs() <= 180.0
            && (lat.abs() >= 0.001 || lon.abs() >= 0.001)
        {
            latitudes.push(lat);
            longitudes.push(lon);
        }
    }
    if latitudes.is_empty() {
        return None;
    }
    latitudes.sort_by(f64::total_cmp);
    longitudes.sort_by(f64::total_cmp);
    Some((
        latitudes[latitudes.len() / 2],
        longitudes[longitudes.len() / 2],
    ))
}

fn normalize_coordinate(value: f64, unit: &str) -> Option<f64> {
    match unit.trim().to_ascii_lowercase().as_str() {
        "deg" | "degree" | "degrees" | "°" => Some(value),
        "rad" | "radian" | "radians" => Some(value.to_degrees()),
        "min" | "arcmin" | "arcminute" => Some(value / 60.0),
        _ => None,
    }
}

fn read_folder_metadata(paths: &[PathBuf]) -> Result<usize, String> {
    let mut by_directory = HashMap::new();
    let mut total_files = 0usize;
    for path in paths {
        let directory = path
            .parent()
            .ok_or_else(|| format!("{} has no parent", path.display()))?;
        if by_directory.contains_key(directory) {
            continue;
        }
        let (document, files) = read_hierarchy(directory)?;
        total_files += files;
        by_directory.insert(directory.to_path_buf(), document);
    }
    std::hint::black_box(by_directory);
    Ok(total_files)
}

fn read_hierarchy(directory: &Path) -> Result<(YamlValue, usize), String> {
    let mut ancestors = directory.ancestors().collect::<Vec<_>>();
    ancestors.reverse();
    let mut merged = YamlValue::Mapping(Default::default());
    let mut files = 0usize;
    for ancestor in ancestors {
        let path = ancestor.join("TRACK.yml");
        if !path.is_file() {
            continue;
        }
        let file = File::open(&path).map_err(display_error("open TRACK.yml"))?;
        let value = serde_yaml::from_reader(BufReader::new(file))
            .map_err(|error| format!("parse {}: {error}", path.display()))?;
        merge_yaml(&mut merged, value);
        files += 1;
    }
    Ok((merged, files))
}

fn merge_yaml(base: &mut YamlValue, overlay: YamlValue) {
    match (base, overlay) {
        (YamlValue::Mapping(base), YamlValue::Mapping(overlay)) => {
            for (key, value) in overlay {
                if let Some(existing) = base.get_mut(&key) {
                    merge_yaml(existing, value);
                } else {
                    base.insert(key, value);
                }
            }
        }
        (base, overlay) => *base = overlay,
    }
}

fn load_cache(path: &Path) -> Result<CacheFile, String> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Ok(CacheFile {
                version: CACHE_VERSION,
                entries: BTreeMap::new(),
            });
        }
        Err(error) => return Err(format!("open cache {}: {error}", path.display())),
    };
    let cache: CacheFile = serde_json::from_reader(BufReader::new(file))
        .map_err(|error| format!("parse cache {}: {error}", path.display()))?;
    if cache.version != CACHE_VERSION {
        return Ok(CacheFile {
            version: CACHE_VERSION,
            entries: BTreeMap::new(),
        });
    }
    Ok(cache)
}

fn save_cache(path: &Path, cache: &CacheFile) -> Result<(), String> {
    let temporary = path.with_extension(format!("tmp.{}", std::process::id()));
    let file = File::create(&temporary).map_err(display_error("create temporary cache"))?;
    let mut writer = BufWriter::new(file);
    serde_json::to_writer(&mut writer, cache).map_err(|error| error.to_string())?;
    writer.flush().map_err(display_error("flush cache"))?;
    writer
        .get_ref()
        .sync_all()
        .map_err(display_error("sync cache"))?;
    drop(writer);
    fs::rename(&temporary, path).map_err(display_error("commit cache"))
}

fn remove_benchmark_cache(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!(
            "remove benchmark cache {}: {error}",
            path.display()
        )),
    }
}

fn validate_inventory(manifest: &Manifest, enabled: bool) -> Result<(), String> {
    if !enabled {
        return Ok(());
    }
    let raw = manifest
        .roots
        .iter()
        .map(|root| root.raw_matches)
        .collect::<Vec<_>>();
    let rows = manifest
        .roots
        .iter()
        .map(|root| root.source_rows)
        .collect::<Vec<_>>();
    let expected_extensions = BTreeMap::from([
        ("ld".to_owned(), 370usize),
        ("mov".to_owned(), 1usize),
        ("mp4".to_owned(), 69usize),
        ("pds".to_owned(), 16usize),
    ]);
    let matches = manifest.roots.len() == 7
        && raw == EXPECTED_ROOT_RAW
        && rows == EXPECTED_ROOT_ROWS
        && manifest.raw_matches == EXPECTED_RAW
        && manifest.source_rows == EXPECTED_ROWS
        && manifest.unique_sources.len() == EXPECTED_UNIQUE
        && manifest.orphan_ldx == EXPECTED_ORPHAN_LDX
        && manifest.ldx_companions == EXPECTED_LDX_COMPANIONS
        && manifest.unique_source_bytes == EXPECTED_SOURCE_BYTES
        && manifest.extensions == expected_extensions
        && manifest.track_metadata_paths.is_empty();
    if matches {
        Ok(())
    } else {
        Err(format!(
            "live inventory changed: roots={} raw={raw:?}/{} rows={rows:?}/{} unique={} bytes={} orphan_ldx={} companions={} extensions={:?} TRACK.yml={} (use --skip-inventory-check only after reviewing it)",
            manifest.roots.len(), manifest.raw_matches, manifest.source_rows,
            manifest.unique_sources.len(), manifest.unique_source_bytes, manifest.orphan_ldx,
            manifest.ldx_companions, manifest.extensions, manifest.track_metadata_paths.len(),
        ))
    }
}

fn validate_pair(cold: &ScanResult, warm: &ScanResult) -> Result<(), String> {
    if cold.unique_sources != warm.unique_sources
        || cold.sessions != warm.sessions
        || cold.unsupported != warm.unsupported
        || cold.fingerprint_bytes != warm.fingerprint_bytes
    {
        return Err("cold and warm scan results disagree".into());
    }
    if cold.cache_hits != 0
        || cold.cache_misses != cold.unique_sources
        || warm.cache_hits != warm.unique_sources
        || warm.cache_misses != 0
        || cold.sessions + cold.unsupported != cold.unique_sources
    {
        return Err(format!(
            "cache accounting failed: cold hits/misses={}/{} warm hits/misses={}/{} sessions={} unsupported={} unique={}",
            cold.cache_hits, cold.cache_misses, warm.cache_hits, warm.cache_misses,
            cold.sessions, cold.unsupported, cold.unique_sources,
        ));
    }
    Ok(())
}

fn print_metrics(results: &[RepetitionResult]) {
    let cold = |select: fn(&ScanResult) -> f64| {
        median(results.iter().map(|run| select(&run.cold)).collect())
    };
    let warm = median(results.iter().map(|run| run.warm.phases.total_ms).collect());
    let representative = &results[results.len() / 2].cold;
    println!(
        "METRIC full_scan_ms={:.3}",
        cold(|scan| scan.phases.total_ms)
    );
    println!("METRIC warm_scan_ms={warm:.3}");
    println!(
        "METRIC discovery_ms={:.3}",
        cold(|scan| scan.phases.discovery_ms)
    );
    println!(
        "METRIC folder_metadata_ms={:.3}",
        cold(|scan| scan.phases.folder_metadata_ms)
    );
    println!(
        "METRIC fingerprint_ms={:.3}",
        cold(|scan| scan.phases.fingerprint_ms)
    );
    println!(
        "METRIC summary_parse_ms={:.3}",
        cold(|scan| scan.phases.summary_parse_ms)
    );
    println!(
        "METRIC cache_serialization_ms={:.3}",
        cold(|scan| scan.phases.cache_serialization_ms)
    );
    println!("METRIC source_count={}", representative.unique_sources);
    println!("METRIC session_count={}", representative.sessions);
    println!("METRIC unsupported_count={}", representative.unsupported);
    println!("METRIC error_count={}", representative.errors);
    println!(
        "METRIC fingerprint_bytes={}",
        representative.fingerprint_bytes
    );
}

fn median(mut values: Vec<f64>) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

fn elapsed_ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1_000.0
}

fn system_time_ms(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn write_json(path: &Path, value: &impl Serialize) -> Result<(), String> {
    let file = File::create(path).map_err(display_error("create JSON report"))?;
    serde_json::to_writer_pretty(BufWriter::new(file), value).map_err(|error| error.to_string())
}

fn display_error(context: &'static str) -> impl FnOnce(io::Error) -> String {
    move |error| format!("{context}: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ldx_resolution_prefers_lowercase_and_rejects_orphans() {
        let directory = tempfile::tempdir().unwrap();
        let ldx = directory.path().join("run.ldx");
        File::create(&ldx).unwrap();
        assert!(telemetry_path_for_input(&ldx).is_none());
        let upper = directory.path().join("run.LD");
        File::create(&upper).unwrap();
        assert_eq!(telemetry_path_for_input(&ldx), Some((upper.clone(), true)));
        let lower = directory.path().join("run.ld");
        File::create(&lower).unwrap();
        assert_eq!(telemetry_path_for_input(&ldx), Some((lower, true)));
    }

    #[test]
    fn candidate_matching_is_case_insensitive_except_track_metadata() {
        for name in ["run.PDS", "run.Ld", "run.LDX", "run.WebM", "TRACK.yml"] {
            assert!(is_candidate(Path::new(name)), "{name}");
        }
        assert!(!is_candidate(Path::new("track.yml")));
        assert!(!is_candidate(Path::new("notes.txt")));
    }

    #[test]
    fn yaml_hierarchy_merges_root_to_leaf() {
        let mut base = serde_yaml::from_str("track:\n  name: Old\n  country: CA\n").unwrap();
        let overlay = serde_yaml::from_str("track:\n  name: New\n").unwrap();
        merge_yaml(&mut base, overlay);
        assert_eq!(base["track"]["name"], "New");
        assert_eq!(base["track"]["country"], "CA");
    }
}
