use motorsport_telemetry::motorsport_telemetry_core::{
    names, Diagnostic, Diagnostics, FileMetadata, Severity, TelemetrySource,
};
use motorsport_telemetry::{
    open, open_metadata, verify, SourceExt, TelemetryError, TelemetryFile, VerifyError, VerifyKind,
    VerifyReport,
};
use serde_json::json;
use std::collections::BTreeSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use telemetry_format::{
    is_jsonl_ext_path, is_jsonl_path, is_jsonl_zstd_path, needs_update, write_from_source,
    write_from_source_stripped, write_jsonl_extension_from_source_with,
    write_jsonl_from_source_with, FORMAT_VERSION,
};
use telemetry_passes::{apply_registry, PassOutcome};

const USAGE: &str = "\
Usage: motorsport-telemetry <command> [options]

Inspect recordings, convert them to .telemetry or JSONL, and verify
the on-disk formats this crate writes.

Commands:
  inspect    Print laps, track, video, identity, and diagnostics
             for a file or every matching recording under a folder
  convert    Write native .telemetry (default) or JSONL by suffix
  verify     Check .telemetry / .telemetry.jsonl / .zstd and flag
             decode faults

Run motorsport-telemetry <command> --help for that command.
Run motorsport-telemetry help <command> for the same text.

Options:
  -h, --help       Show this help
  -V, --version    Show version
";

const INSPECT_HELP: &str = "\
Usage: motorsport-telemetry inspect [options] <path>
Print laps, track, video, and identity. <path> may be one recording or a
folder. A folder is walked recursively; only recognized telemetry names
are considered, then --mask (if any) filters that set.

A diagnostics: section lists problems the reader recovered from and
plausibility findings from the validator; `none` means a clean file. With
--json each file report carries a `diagnostics` array of {severity, code,
channel, message} objects.

Arguments:
  <path>               A telemetry file, or a directory to scan

Options:
  --json               Machine-readable JSON
  -m, --mask <glob>    Keep files whose relative path or file name matches
                       this glob. Repeatable; a file matches if any mask
                       matches. Matching is case-insensitive. Use / in globs.
  -h, --help           Show this help

Globs:
  *                    Any characters except /
  **                   Any directories, including none
  ?                    One character except /
  {ld,pds}             Either alternative

Default folder filter (no --mask):
  .mp4  .pds  .ld  .vbo  .telemetry
  .telemetry.jsonl  .jsonl  .mtj  .telemetry.ext.jsonl
  and those names with .zstd / .zst

A single file still prints one report. A folder prints one report per
match; --json then wraps them:

  {\"root\",\"mask\",\"ok\",\"failed\",\"files\":[...],\"errors\":[...]}

Examples:
  motorsport-telemetry inspect run.ld
  motorsport-telemetry inspect --json run.mp4
  motorsport-telemetry inspect ~/Documents/Telemetry
  motorsport-telemetry inspect ~/Documents/Telemetry --mask '**/*.pds'
  motorsport-telemetry inspect ~/Documents --mask '**/sebring-2026/**' --mask '*.telemetry'
";

const CONVERT_HELP: &str = "\
Usage: motorsport-telemetry convert [options] <input> [output]

Convert a recording to native .telemetry, or to JSONL when the output
name says so. By default the standard processing-pass registry runs and
appends its derived channels; every pass is lossless, and --strip-passes
recovers a byte-identical raw conversion.

Arguments:
  <input>              Any supported source: .mp4 .pds .ld .vbo
                       .telemetry .telemetry.jsonl and .zstd / .zst variants
  [output]             Destination. Default: <input-file-name>.telemetry
                       next to the input file

Options:
  --no-passes          Convert the source as-is; run no passes
  --strip-passes       Drop previously applied pass outputs (native
                       .telemetry output only)

Output suffix:
  .telemetry                    Native STORE zip (the default)
  .telemetry.jsonl              MTJ, uncompressed UTF-8
  .telemetry.jsonl.zstd         MTJ, one zstd frame (level 11)
  .telemetry.ext.jsonl[.zstd]   MTX sidecar

The printed line is the destination path.

Examples:
  motorsport-telemetry convert run.pds
  motorsport-telemetry convert run.pds weekend.telemetry
  motorsport-telemetry convert run.pds weekend.telemetry.jsonl
  motorsport-telemetry convert run.pds weekend.telemetry.jsonl.zstd
  motorsport-telemetry convert run.pds overlay.telemetry.ext.jsonl
";

const VERIFY_HELP: &str = "\
Usage: motorsport-telemetry verify <file>...

Check that each file is a valid native .telemetry archive or an MTJ/MTX
JSONL document (plain or zstd). Opens a native file without rewriting an
older catalog. Decodes one sample from every channel.

After the format check, reader diagnostics and plausibility findings are
printed for every file. A file whose channels claim more sample bytes than
the file holds, or whose decoded values are absurdly large, is a decode
fault (the bytes were read at the wrong width) and fails even though it
opened. Plain warnings do not fail the command.

Accepted names:
  .telemetry
  .telemetry.jsonl  .jsonl  .mtj
  .telemetry.ext.jsonl
  those names with .zstd / .zst

A compressed frame still verifies under a .telemetry.jsonl name (zstd
magic is sniffed). Vendor files (.pds .ld .mp4 .vbo) are rejected.

Exit status is 1 if any file fails or is a decode fault, 2 on usage errors.

Examples:
  motorsport-telemetry verify run.telemetry
  motorsport-telemetry verify run.telemetry.jsonl run.telemetry.jsonl.zstd
";

const SUSPICIOUS_CLOCK_AGE_DAYS: i64 = 365 * 2;

#[derive(Debug)]
struct Inspection {
    file: String,
    format: String,
    format_version: Option<u16>,
    format_needs_update: Option<bool>,
    source_format: String,
    source_path: String,
    passes: Vec<String>,
    driver_ids: Vec<i64>,
    laps: usize,
    complete_laps: usize,
    fastest_lap_ns: Option<u64>,
    fastest_lap_number: Option<i64>,
    video_included: bool,
    video_filenames: Vec<String>,
    video_file_indices: Vec<u32>,
    video_presentation_offset_ns: Option<i128>,
    session_key: Option<String>,
    car_type: Option<String>,
    car_number: Option<String>,
    car_class: Option<String>,
    track_gps: Option<(f64, f64)>,
    track_name: Option<String>,
    layout: Option<String>,
    track_length_m: Option<f64>,
    event_date: Option<String>,
    event_date_source: Option<String>,
    event_date_warning: Option<String>,
    diagnostics: Vec<Diagnostic>,
}

fn main() {
    match arguments(std::env::args_os().skip(1)) {
        Ok(Command::Help { topic }) => print!("{}", help_text(topic)),
        Ok(Command::Version) => println!("motorsport-telemetry {}", env!("CARGO_PKG_VERSION")),
        Ok(Command::Inspect { path, json, masks }) => {
            if let Err(error) = run_inspect(&path, json, &masks) {
                eprintln!("motorsport-telemetry: {error}");
                std::process::exit(1);
            }
        }
        Ok(Command::Convert {
            input,
            output,
            passes,
        }) => match convert(&input, output.as_deref(), passes) {
            Ok(dest) => println!("{}", dest.display()),
            Err(error) => {
                eprintln!("motorsport-telemetry: {error}");
                std::process::exit(1);
            }
        },
        Ok(Command::Verify { paths }) => {
            let mut failed = 0usize;
            for path in &paths {
                match verify(path) {
                    Ok(report) => println!("{}", format_verify_report(path, &report)),
                    Err(VerifyError::DecodeFault(diagnostics)) => {
                        failed += 1;
                        eprintln!(
                            "{}: FAIL  {}",
                            path.display(),
                            decode_fault_message(&diagnostics)
                        );
                    }
                    Err(error) => {
                        failed += 1;
                        eprintln!("{}: FAIL  {error}", path.display());
                    }
                }
            }
            if failed > 0 {
                std::process::exit(1);
            }
        }
        Err(message) => {
            eprintln!("motorsport-telemetry: {message}");
            eprintln!();
            eprint!("{USAGE}");
            std::process::exit(2);
        }
    }
}

fn help_text(topic: HelpTopic) -> &'static str {
    match topic {
        HelpTopic::Root => USAGE,
        HelpTopic::Inspect => INSPECT_HELP,
        HelpTopic::Convert => CONVERT_HELP,
        HelpTopic::Verify => VERIFY_HELP,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum HelpTopic {
    Root,
    Inspect,
    Convert,
    Verify,
}

#[derive(Debug, PartialEq, Eq)]
enum Command {
    Help {
        topic: HelpTopic,
    },
    Version,
    Inspect {
        path: PathBuf,
        json: bool,
        masks: Vec<String>,
    },
    Convert {
        input: PathBuf,
        output: Option<PathBuf>,
        passes: PassMode,
    },
    Verify {
        paths: Vec<PathBuf>,
    },
}

/// What `convert` does with the processing-pass registry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PassMode {
    /// Run every applicable registered pass and append its channels.
    Apply,
    /// Convert the source as-is, applying nothing.
    Skip,
    /// Drop previously applied pass outputs, recovering the raw conversion.
    Strip,
}

fn arguments(args: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    let mut args = args.into_iter().peekable();
    let Some(first) = args.next() else {
        return Err("missing command".into());
    };
    if first == "-h" || first == "--help" {
        return Ok(Command::Help {
            topic: HelpTopic::Root,
        });
    }
    if first == "-V" || first == "--version" {
        return Ok(Command::Version);
    }
    match first.to_string_lossy().as_ref() {
        "help" => parse_help_topic(args),
        "inspect" => parse_inspect(args),
        "convert" => parse_convert(args),
        "verify" => parse_verify(args),
        other if other.starts_with('-') => Err(format!("unknown option {other}")),
        other => Err(format!(
            "unknown command {other} (try inspect, convert, or verify)"
        )),
    }
}

fn parse_help_topic(args: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    let mut topic = HelpTopic::Root;
    for argument in args {
        let name = argument.to_string_lossy();
        if name == "-h" || name == "--help" {
            return Ok(Command::Help {
                topic: HelpTopic::Root,
            });
        }
        topic = match name.as_ref() {
            "inspect" => HelpTopic::Inspect,
            "convert" => HelpTopic::Convert,
            "verify" => HelpTopic::Verify,
            other => return Err(format!("no help topic {other}")),
        };
    }
    Ok(Command::Help { topic })
}

fn parse_inspect(args: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    let mut path = None;
    let mut json = false;
    let mut masks = Vec::new();
    let mut args = args.into_iter().peekable();
    while let Some(argument) = args.next() {
        if argument == "-h" || argument == "--help" {
            return Ok(Command::Help {
                topic: HelpTopic::Inspect,
            });
        }
        if argument == "--json" {
            json = true;
            continue;
        }
        let text = argument.to_string_lossy();
        if text == "-m" || text == "--mask" {
            let value = args
                .next()
                .ok_or_else(|| "inspect --mask needs a glob".to_string())?;
            masks.push(value.to_string_lossy().into_owned());
            continue;
        }
        if let Some(value) = text.strip_prefix("--mask=") {
            if value.is_empty() {
                return Err("inspect --mask needs a glob".into());
            }
            masks.push(value.to_owned());
            continue;
        }
        if text.starts_with('-') {
            return Err(format!("unknown option {text}"));
        }
        if path.replace(PathBuf::from(&argument)).is_some() {
            return Err("inspect expects one file or folder".into());
        }
    }
    path.map(|path| Command::Inspect { path, json, masks })
        .ok_or_else(|| "inspect is missing a file or folder".into())
}

fn parse_convert(args: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    let mut positional = Vec::new();
    let mut passes = PassMode::Apply;
    for argument in args {
        if argument == "-h" || argument == "--help" {
            return Ok(Command::Help {
                topic: HelpTopic::Convert,
            });
        }
        if argument == "--no-passes" {
            passes = PassMode::Skip;
            continue;
        }
        if argument == "--strip-passes" {
            passes = PassMode::Strip;
            continue;
        }
        if argument.to_string_lossy().starts_with('-') {
            return Err(format!("unknown option {}", argument.to_string_lossy()));
        }
        positional.push(PathBuf::from(argument));
    }
    match positional.as_slice() {
        [input] => Ok(Command::Convert {
            input: input.clone(),
            output: None,
            passes,
        }),
        [input, output] => Ok(Command::Convert {
            input: input.clone(),
            output: Some(output.clone()),
            passes,
        }),
        [] => Err("convert is missing an input file".into()),
        _ => Err("convert expects <input> [output]".into()),
    }
}

fn parse_verify(args: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    let mut paths = Vec::new();
    for argument in args {
        if argument == "-h" || argument == "--help" {
            return Ok(Command::Help {
                topic: HelpTopic::Verify,
            });
        }
        if argument.to_string_lossy().starts_with('-') {
            return Err(format!("unknown option {}", argument.to_string_lossy()));
        }
        paths.push(PathBuf::from(argument));
    }
    if paths.is_empty() {
        return Err("verify is missing a telemetry file".into());
    }
    Ok(Command::Verify { paths })
}

fn run_inspect(path: &Path, json: bool, masks: &[String]) -> Result<(), String> {
    let targets = collect_inspect_targets(path, masks)?;
    if targets.is_empty() {
        return Err(if masks.is_empty() {
            format!("no telemetry files under {}", path.display())
        } else {
            format!(
                "no telemetry files under {} matching {}",
                path.display(),
                masks.join(", ")
            )
        });
    }
    if targets.len() == 1 && path.is_file() {
        let inspection = inspect(&targets[0]).map_err(|error| error.to_string())?;
        if json {
            print_json(&inspection);
        } else {
            print_human(&inspection);
        }
        return Ok(());
    }

    let mut files = Vec::new();
    let mut errors = Vec::new();
    for (index, target) in targets.iter().enumerate() {
        match inspect(target) {
            Ok(inspection) => {
                if !json {
                    if index > 0 {
                        println!("---");
                    }
                    println!("# {}/{}  {}", index + 1, targets.len(), target.display());
                    print_human(&inspection);
                }
                files.push(inspection);
            }
            Err(error) => {
                let message = error.to_string();
                if json {
                    errors.push((target.display().to_string(), message));
                } else {
                    println!("# {}/{}  {}", index + 1, targets.len(), target.display());
                    println!("error: {message}");
                    errors.push((target.display().to_string(), message));
                }
            }
        }
    }
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "root": path.display().to_string(),
                "mask": masks,
                "ok": files.len(),
                "failed": errors.len(),
                "files": files.iter().map(inspection_json).collect::<Vec<_>>(),
                "errors": errors.iter().map(|(file, error)| json!({
                    "file": file,
                    "error": error,
                })).collect::<Vec<_>>(),
            }))
            .expect("scan JSON is serializable")
        );
    } else {
        println!(
            "---\nscanned {} file{}, {} error{}",
            files.len() + errors.len(),
            if files.len() + errors.len() == 1 {
                ""
            } else {
                "s"
            },
            errors.len(),
            if errors.len() == 1 { "" } else { "s" }
        );
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!("{} file(s) failed to inspect", errors.len()))
    }
}

fn collect_inspect_targets(root: &Path, masks: &[String]) -> Result<Vec<PathBuf>, String> {
    if root.is_file() {
        if !masks.is_empty() && !matches_any_mask(root, root, masks) {
            return Ok(Vec::new());
        }
        return Ok(vec![root.to_path_buf()]);
    }
    if !root.is_dir() {
        return Err(format!("not a file or directory: {}", root.display()));
    }
    let mut out = Vec::new();
    walk_inspect(root, root, masks, &mut out).map_err(|error| error.to_string())?;
    out.sort();
    Ok(out)
}

fn walk_inspect(
    root: &Path,
    dir: &Path,
    masks: &[String],
    out: &mut Vec<PathBuf>,
) -> std::io::Result<()> {
    let mut entries: Vec<_> = fs::read_dir(dir)?.filter_map(Result::ok).collect();
    entries.sort_by_key(|entry| entry.path());
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        if name == ".git" {
            continue;
        }
        let metadata = match entry.file_type() {
            Ok(file_type) if file_type.is_symlink() => match fs::metadata(&path) {
                Ok(metadata) => metadata,
                Err(_) => continue,
            },
            Ok(_) => match entry.metadata() {
                Ok(metadata) => metadata,
                Err(_) => continue,
            },
            Err(_) => continue,
        };
        if metadata.is_dir() {
            if path
                .symlink_metadata()
                .is_ok_and(|m| m.file_type().is_symlink())
            {
                continue;
            }
            walk_inspect(root, &path, masks, out)?;
        } else if metadata.is_file()
            && is_known_telemetry_path(&path)
            && (masks.is_empty() || matches_any_mask(root, &path, masks))
        {
            out.push(path);
        }
    }
    Ok(())
}

fn is_known_telemetry_path(path: &Path) -> bool {
    if is_jsonl_path(path) || is_native_telemetry(path) {
        return true;
    }
    matches!(
        path.extension()
            .and_then(|extension| extension.to_str())
            .map(|extension| extension.to_ascii_lowercase())
            .as_deref(),
        Some("mp4" | "pds" | "ld" | "vbo")
    )
}

fn matches_any_mask(root: &Path, path: &Path, masks: &[String]) -> bool {
    let relative = path.strip_prefix(root).unwrap_or(path);
    let relative = relative.to_string_lossy().replace('\\', "/");
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    masks.iter().any(|mask| {
        expand_braces(mask)
            .into_iter()
            .any(|pattern| glob_match(&pattern, &relative) || glob_match(&pattern, &name))
    })
}

fn expand_braces(pattern: &str) -> Vec<String> {
    let Some(start) = pattern.find('{') else {
        return vec![pattern.to_owned()];
    };
    let Some(end) = pattern[start + 1..]
        .find('}')
        .map(|index| start + 1 + index)
    else {
        return vec![pattern.to_owned()];
    };
    if pattern[start + 1..end].contains('{') {
        return vec![pattern.to_owned()];
    }
    let prefix = &pattern[..start];
    let suffix = &pattern[end + 1..];
    let mut out = Vec::new();
    for alt in pattern[start + 1..end].split(',') {
        for expanded in expand_braces(&format!("{prefix}{alt}{suffix}")) {
            out.push(expanded);
        }
    }
    if out.is_empty() {
        vec![pattern.to_owned()]
    } else {
        out
    }
}

fn glob_match(pattern: &str, text: &str) -> bool {
    let pattern = pattern.trim_start_matches("./").to_ascii_lowercase();
    let text = text.trim_start_matches("./").to_ascii_lowercase();
    glob_match_bytes(pattern.as_bytes(), text.as_bytes())
}

fn glob_match_bytes(pattern: &[u8], text: &[u8]) -> bool {
    if pattern.is_empty() {
        return text.is_empty();
    }
    if pattern == b"**" {
        return true;
    }
    if pattern.starts_with(b"**/") {
        return glob_match_bytes(&pattern[3..], text)
            || text
                .iter()
                .position(|&byte| byte == b'/')
                .is_some_and(|slash| glob_match_bytes(pattern, &text[slash + 1..]));
    }
    if pattern.starts_with(b"**") {
        return glob_match_bytes(&pattern[2..], text)
            || (!text.is_empty() && glob_match_bytes(pattern, &text[1..]));
    }
    if pattern[0] == b'*' {
        if glob_match_bytes(&pattern[1..], text) {
            return true;
        }
        return !text.is_empty() && text[0] != b'/' && glob_match_bytes(pattern, &text[1..]);
    }
    if pattern[0] == b'?' {
        return !text.is_empty() && text[0] != b'/' && glob_match_bytes(&pattern[1..], &text[1..]);
    }
    !text.is_empty() && pattern[0] == text[0] && glob_match_bytes(&pattern[1..], &text[1..])
}

fn convert(
    input: &Path,
    output: Option<&Path>,
    passes: PassMode,
) -> Result<PathBuf, TelemetryError> {
    let dest = output
        .map(Path::to_path_buf)
        .unwrap_or_else(|| default_telemetry_dest(input));
    let file = open(input)?;
    match passes {
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
            write_converted(&passed, &dest)?;
        }
        PassMode::Skip => write_converted(&file, &dest)?,
        PassMode::Strip => {
            if is_jsonl_path(&dest) {
                return Err(TelemetryError::Unsupported(
                    "--strip-passes writes .telemetry output only".into(),
                ));
            }
            if dest == input {
                // In-place strip: write next to the destination, then swap.
                let dir = dest.parent().unwrap_or_else(|| Path::new("."));
                let name = dest
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("recording");
                let temp = dir.join(format!(".{name}.strip-tmp"));
                write_from_source_stripped(&file, &temp)?;
                drop(file);
                fs::rename(&temp, &dest)
                    .map_err(telemetry_format::TelemetryFormatError::Io)
                    .map_err(TelemetryError::Telemetry)?;
            } else {
                write_from_source_stripped(&file, &dest)?;
            }
        }
    }
    Ok(dest)
}

fn write_converted(source: &dyn TelemetrySource, dest: &Path) -> Result<(), TelemetryError> {
    if is_jsonl_path(dest) {
        let compress = is_jsonl_zstd_path(dest);
        if is_jsonl_ext_path(dest) {
            write_jsonl_extension_from_source_with(source, dest, compress)?;
        } else {
            write_jsonl_from_source_with(source, dest, compress)?;
        }
    } else {
        write_from_source(source, dest)?;
    }
    Ok(())
}

fn default_telemetry_dest(src: &Path) -> PathBuf {
    let mut dest = src.to_path_buf();
    let name = src
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("recording");
    dest.set_file_name(format!("{name}.telemetry"));
    dest
}

fn is_native_telemetry(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("telemetry"))
}

/// Formats a [`VerifyReport`] as the one-line `verify` success message.
fn format_verify_report(path: &Path, report: &VerifyReport) -> String {
    let utc = report
        .utc_start_ns
        .map(|utc| utc.to_string())
        .unwrap_or_else(|| "none".into());
    let mut out = match report.kind {
        VerifyKind::Native => format!(
            "{}: ok  native v{}  channels={} laps={} utc={}{}",
            path.display(),
            report.format_version.unwrap_or(0),
            report.channels,
            report.laps,
            utc,
            if report.needs_update {
                format!("  needs_update (current v{FORMAT_VERSION})")
            } else {
                String::new()
            }
        ),
        VerifyKind::Mtj | VerifyKind::Mtx => {
            let kind = match report.kind {
                VerifyKind::Mtj => "mtj",
                _ => "mtx",
            };
            let extra = if matches!(report.kind, VerifyKind::Mtx) {
                format!("  groups={}", report.sidecar_groups)
            } else {
                format!("  laps={}", report.laps)
            };
            format!(
                "{}: ok  {kind}:{}{}  channels={} spans={} utc={} q={}{}",
                path.display(),
                report.jsonl_version.unwrap_or(0),
                if report.compressed { "  zstd" } else { "" },
                report.channels,
                report.spans,
                utc,
                report.quantum_ns,
                extra
            )
        }
    };
    out.push_str(&format_diagnostics_block(report.diagnostics.items()));
    out
}

/// One-line count of diagnostics by severity, or `none` when empty.
fn diagnostics_summary(diagnostics: &[Diagnostic]) -> String {
    if diagnostics.is_empty() {
        return "none".into();
    }
    let mut info = 0usize;
    let mut warnings = 0usize;
    let mut errors = 0usize;
    for diagnostic in diagnostics {
        match diagnostic.severity {
            Severity::Info => info += 1,
            Severity::Warning => warnings += 1,
            Severity::Error => errors += 1,
        }
    }
    let mut parts = Vec::new();
    if info > 0 {
        parts.push(format!("{info} info{}", if info == 1 { "" } else { "s" }));
    }
    if warnings > 0 {
        parts.push(format!(
            "{warnings} warning{}",
            if warnings == 1 { "" } else { "s" }
        ));
    }
    if errors > 0 {
        parts.push(format!(
            "{errors} error{}",
            if errors == 1 { "" } else { "s" }
        ));
    }
    if parts.is_empty() {
        "none".into()
    } else {
        parts.join(", ")
    }
}

/// Renders a `diagnostics:` summary line plus one indented line per finding,
/// prefixed with a newline so it can be appended to an existing report.
fn format_diagnostics_block(items: &[Diagnostic]) -> String {
    if items.is_empty() {
        return String::new();
    }
    let mut out = format!("\ndiagnostics: {}", diagnostics_summary(items));
    for diagnostic in items {
        out.push_str(&format!("\n  {diagnostic}"));
    }
    out
}

/// The failure message printed when a file's diagnostics imply a decode fault.
fn decode_fault_message(diagnostics: &Diagnostics) -> String {
    let mut message =
        String::from("decode fault: at least one channel was decoded at the wrong sample width");
    message.push_str(&format_diagnostics_block(diagnostics.items()));
    message
}

fn inspect(path: &Path) -> Result<Inspection, motorsport_telemetry::TelemetryError> {
    let file = open_for_inspection(path)?;
    let metadata = file.metadata();
    let track = file.match_track();
    let track_gps = track.as_ref().map(|context| context.gps);
    let (video_included, video_file_indices) = video_info(&file, &metadata);
    let video_filenames = if !metadata.videos.is_empty() {
        // Linked files recorded in the catalog (or header): the actual video
        // names, not the telemetry container that carries the linkage.
        metadata
            .videos
            .iter()
            .map(|video| video.filename.clone())
            .collect()
    } else if metadata.video_frame_count.is_some() {
        path.file_name()
            .map(|name| vec![name.to_string_lossy().into_owned()])
            .unwrap_or_default()
    } else if video_included {
        nearby_video_filenames(path)
    } else {
        Vec::new()
    };
    let identity = &metadata.identity;
    let car_type = nonempty(&identity.vehicle).or_else(|| {
        first_semantic_value(
            &file,
            &["cartype", "vehicletype", "vehiclemodel", "carmodel"],
        )
    });
    let car_number = first_semantic_value(
        &file,
        &[
            "carnumber",
            "vehiclenumber",
            "racenumber",
            "competitionnumber",
        ],
    );
    let car_class = first_semantic_value(
        &file,
        &["carclass", "vehicleclass", "classid", "competitionclass"],
    );
    let event_date = event_date(path, &metadata);
    let diagnostics = file.validate().into_items();

    Ok(Inspection {
        file: path.to_string_lossy().into_owned(),
        format: metadata.format.clone(),
        format_version: metadata.format_version,
        format_needs_update: metadata.format_version.map(needs_update),
        source_format: metadata.source_format.clone(),
        source_path: metadata.source_path.clone(),
        passes: metadata
            .passes
            .iter()
            .map(motorsport_telemetry::motorsport_telemetry_core::AppliedPass::label)
            .collect(),
        driver_ids: metadata.driver_ids.clone(),
        laps: metadata.laps.len(),
        complete_laps: metadata.laps.iter().filter(|lap| lap.complete).count(),
        fastest_lap_ns: metadata.fastest_lap.as_ref().map(|lap| lap.duration_ns),
        fastest_lap_number: metadata.fastest_lap.as_ref().map(|lap| lap.number),
        video_included,
        video_filenames,
        video_file_indices,
        video_presentation_offset_ns: metadata.video_presentation_offset_ns,
        session_key: metadata.session_key.clone(),
        car_type,
        car_number,
        car_class,
        track_gps,
        track_name: track
            .as_ref()
            .map(|context| context.matched.track.name.to_owned())
            .or_else(|| nonempty(&identity.venue)),
        layout: track
            .as_ref()
            .map(|context| context.matched.layout.name.to_owned()),
        track_length_m: track.and_then(|context| context.matched.layout.length_m),
        event_date: event_date.selected.map(|date| date.to_string()),
        event_date_source: event_date.source,
        event_date_warning: event_date.warning,
        diagnostics,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CivilDate {
    year: i32,
    month: u32,
    day: u32,
}

impl std::fmt::Display for CivilDate {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "{:04}-{:02}-{:02}",
            self.year, self.month, self.day
        )
    }
}

#[derive(Debug)]
struct EventDate {
    selected: Option<CivilDate>,
    source: Option<String>,
    warning: Option<String>,
}

fn event_date(path: &Path, metadata: &FileMetadata) -> EventDate {
    let telemetry = telemetry_date(metadata);
    let created = std::fs::metadata(path)
        .and_then(|metadata| metadata.created())
        .ok()
        .and_then(date_from_system_time);
    select_event_date(telemetry, created)
}

fn telemetry_date(metadata: &FileMetadata) -> Option<(CivilDate, String)> {
    let clock_date = metadata
        .absolute_clock
        .as_deref()
        .filter(|clock| *clock != "time_of_day")
        .and(metadata.absolute_start_ns)
        .and_then(date_from_unix_ns)
        .filter(plausible_date)
        .map(|date| {
            let clock = metadata.absolute_clock.as_deref().unwrap_or("absolute");
            (date, format!("{clock}_clock"))
        });
    clock_date.or_else(|| {
        parse_source_date(&metadata.identity.date)
            .filter(plausible_date)
            .map(|date| (date, "embedded_date".into()))
    })
}

fn select_event_date(
    telemetry: Option<(CivilDate, String)>,
    created: Option<CivilDate>,
) -> EventDate {
    if let (Some((telemetry_date, telemetry_source)), Some(created_date)) = (&telemetry, created) {
        let age_days = days_from_civil(created_date) - days_from_civil(*telemetry_date);
        let future_days = -age_days;
        if age_days >= SUSPICIOUS_CLOCK_AGE_DAYS {
            return EventDate {
                selected: Some(created_date),
                source: Some("file_created_at".into()),
                warning: Some(format!(
                    "rejected {telemetry_source} date {telemetry_date}: {age_days} days older than file creation"
                )),
            };
        }
        if future_days > 7 {
            return EventDate {
                selected: Some(created_date),
                source: Some("file_created_at".into()),
                warning: Some(format!(
                    "rejected {telemetry_source} date {telemetry_date}: {future_days} days newer than file creation"
                )),
            };
        }
    }
    if let Some((date, source)) = telemetry {
        EventDate {
            selected: Some(date),
            source: Some(source),
            warning: None,
        }
    } else {
        EventDate {
            selected: created,
            source: created.map(|_| "file_created_at".into()),
            warning: None,
        }
    }
}

fn plausible_date(date: &CivilDate) -> bool {
    (1980..=2200).contains(&date.year)
}

fn parse_source_date(value: &str) -> Option<CivilDate> {
    let separator = if value.contains('/') { '/' } else { '-' };
    let parts = value
        .split(separator)
        .map(str::trim)
        .map(str::parse::<i32>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    let [first, second, third] = parts.as_slice() else {
        return None;
    };
    let (year, month, day) = if *first >= 1000 {
        (
            *first,
            u32::try_from(*second).ok()?,
            u32::try_from(*third).ok()?,
        )
    } else {
        (
            *third,
            u32::try_from(*second).ok()?,
            u32::try_from(*first).ok()?,
        )
    };
    valid_civil_date(CivilDate { year, month, day })
}

fn valid_civil_date(date: CivilDate) -> Option<CivilDate> {
    let leap = date.year % 4 == 0 && (date.year % 100 != 0 || date.year % 400 == 0);
    let days = match date.month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => return None,
    };
    (date.day > 0 && date.day <= days).then_some(date)
}

fn date_from_system_time(time: SystemTime) -> Option<CivilDate> {
    let seconds = time.duration_since(UNIX_EPOCH).ok()?.as_secs();
    date_from_unix_seconds(seconds)
}

fn date_from_unix_ns(timestamp_ns: u64) -> Option<CivilDate> {
    date_from_unix_seconds(timestamp_ns / 1_000_000_000)
}

fn date_from_unix_seconds(seconds: u64) -> Option<CivilDate> {
    let days = i64::try_from(seconds / 86_400).ok()?;
    civil_from_days(days)
}

fn days_from_civil(date: CivilDate) -> i64 {
    let year = i64::from(date.year) - i64::from(date.month <= 2);
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let month = i64::from(date.month) + if date.month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month + 2) / 5 + i64::from(date.day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

fn civil_from_days(days: i64) -> Option<CivilDate> {
    let shifted = days.checked_add(719_468)?;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    valid_civil_date(CivilDate {
        year: i32::try_from(year).ok()?,
        month: u32::try_from(month).ok()?,
        day: u32::try_from(day).ok()?,
    })
}

fn open_for_inspection(path: &Path) -> Result<TelemetryFile, motorsport_telemetry::TelemetryError> {
    if path
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("vbo"))
    {
        open_metadata(path)
    } else {
        open(path)
    }
}

fn nonempty(value: &str) -> Option<String> {
    (!value.trim().is_empty()).then(|| value.trim().to_owned())
}

fn first_semantic_value(file: &TelemetryFile, candidates: &[&str]) -> Option<String> {
    let channels = file.channels();
    let index = names::find(channels, candidates)?;
    let channel = &channels[index];
    if channel.sample_count == 0 || channel.chunks.is_empty() {
        return None;
    }
    let value = file.decode(index, 0, 0);
    if !value.is_finite() {
        return None;
    }
    Some(if (value - value.round()).abs() < 1e-9 {
        format!("{value:.0}")
    } else {
        value.to_string()
    })
}

fn channel_values(file: &TelemetryFile, candidates: &[&str]) -> Vec<f64> {
    let Some(index) = names::find(file.channels(), candidates) else {
        return Vec::new();
    };
    let mut values = Vec::new();
    for (chunk_index, chunk) in file.channels()[index].chunks.iter().enumerate() {
        values.extend(
            (0..chunk.sample_count).map(|local_index| file.decode(index, chunk_index, local_index)),
        );
    }
    values
}

fn video_info(file: &TelemetryFile, metadata: &FileMetadata) -> (bool, Vec<u32>) {
    if metadata.video_frame_count.is_some() {
        return (true, Vec::new());
    }
    let indices = channel_values(file, &["avifileindex"])
        .into_iter()
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map(|value| value.round() as u32)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let sync_present = channel_values(file, &["avisynctime", "avitime"])
        .into_iter()
        .any(|value| value.is_finite() && value.abs() > f64::EPSILON);
    let linked = sync_present || indices.iter().any(|index| *index > 0);
    (linked, if linked { indices } else { Vec::new() })
}

fn nearby_video_filenames(path: &Path) -> Vec<String> {
    let Some(parent) = path.parent() else {
        return Vec::new();
    };
    let source_stem = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    let mut files = parent
        .read_dir()
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let candidate = entry.path();
            let extension = candidate
                .extension()?
                .to_string_lossy()
                .to_ascii_lowercase();
            if !matches!(extension.as_str(), "avi" | "mp4" | "mov" | "mkv") {
                return None;
            }
            let stem = candidate
                .file_stem()?
                .to_string_lossy()
                .to_ascii_lowercase();
            (stem == source_stem || stem.starts_with(&format!("{source_stem}_"))).then(|| {
                candidate
                    .file_name()
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
        })
        .collect::<Vec<_>>();
    files.sort();
    files
}

fn print_human(inspection: &Inspection) {
    println!("file: {}", inspection.file);
    println!("format: {}", inspection.format);
    if let Some(version) = inspection.format_version {
        println!("format_version: {version}");
        println!(
            "format_needs_update: {}",
            inspection.format_needs_update.unwrap_or(false)
        );
    }
    println!("source_format: {}", inspection.source_format);
    println!("source_path: {}", inspection.source_path);
    println!(
        "passes: {}",
        if inspection.passes.is_empty() {
            "none".into()
        } else {
            inspection.passes.join(", ")
        }
    );
    println!(
        "event_date: {}",
        inspection.event_date.as_deref().unwrap_or("unknown")
    );
    println!(
        "event_date_source: {}",
        inspection.event_date_source.as_deref().unwrap_or("unknown")
    );
    if let Some(warning) = &inspection.event_date_warning {
        println!("event_date_warning: {warning}");
    }
    println!("driver_id: {}", display_ids(&inspection.driver_ids));
    println!("laps: {}", inspection.laps);
    println!("complete_laps: {}", inspection.complete_laps);
    println!(
        "fastest_lap: {}",
        inspection
            .fastest_lap_ns
            .map(format_duration)
            .unwrap_or_else(|| "unknown".into())
    );
    println!(
        "fastest_lap_number: {}",
        display(inspection.fastest_lap_number)
    );
    println!("video_included: {}", inspection.video_included);
    println!(
        "video_filenames: {}",
        if inspection.video_included {
            display_strings(&inspection.video_filenames)
        } else {
            "none".into()
        }
    );
    if !inspection.video_file_indices.is_empty() {
        println!(
            "video_file_indices: {}",
            inspection
                .video_file_indices
                .iter()
                .map(u32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    println!(
        "video_presentation_offset_ns: {}",
        display(inspection.video_presentation_offset_ns)
    );
    println!("part_of_larger_session: unknown (single-file inspection)");
    println!(
        "session_key: {}",
        inspection.session_key.as_deref().unwrap_or("unknown")
    );
    println!(
        "car_type: {}",
        inspection.car_type.as_deref().unwrap_or("unknown")
    );
    println!(
        "car_number: {}",
        inspection.car_number.as_deref().unwrap_or("unknown")
    );
    println!(
        "car_class: {}",
        inspection.car_class.as_deref().unwrap_or("unknown")
    );
    println!(
        "track_gps: {}",
        inspection.track_gps.map_or_else(
            || "unknown".into(),
            |(latitude, longitude)| format!("{latitude:.6}, {longitude:.6}")
        )
    );
    println!(
        "track_name: {}",
        inspection.track_name.as_deref().unwrap_or("unknown")
    );
    println!(
        "layout: {}",
        inspection.layout.as_deref().unwrap_or("unknown")
    );
    println!(
        "track_length: {}",
        inspection
            .track_length_m
            .map_or_else(|| "unknown".into(), |length| format!("{length:.0} m"))
    );
    println!(
        "diagnostics: {}",
        diagnostics_summary(&inspection.diagnostics)
    );
    for diagnostic in &inspection.diagnostics {
        println!("  {diagnostic}");
    }
}

fn print_json(inspection: &Inspection) {
    println!(
        "{}",
        serde_json::to_string_pretty(&inspection_json(inspection))
            .expect("inspection JSON is serializable")
    );
}

fn inspection_json(inspection: &Inspection) -> serde_json::Value {
    let video_filenames = inspection
        .video_included
        .then_some(&inspection.video_filenames)
        .filter(|filenames| !filenames.is_empty());
    json!({
        "file": inspection.file,
        "format": inspection.format,
        "format_version": inspection.format_version,
        "format_needs_update": inspection.format_needs_update,
        "source_format": inspection.source_format,
        "source_path": inspection.source_path,
        "passes": inspection.passes,
        "event_date": inspection.event_date,
        "event_date_source": inspection.event_date_source,
        "event_date_warning": inspection.event_date_warning,
        "driver_id": inspection.driver_ids.first(),
        "driver_ids": inspection.driver_ids,
        "laps": inspection.laps,
        "complete_laps": inspection.complete_laps,
        "fastest_lap_ns": inspection.fastest_lap_ns,
        "fastest_lap": inspection.fastest_lap_ns.map(format_duration),
        "fastest_lap_number": inspection.fastest_lap_number,
        "video_included": inspection.video_included,
        "video_filenames": video_filenames,
        "video_file_indices": inspection.video_file_indices,
        "video_presentation_offset_ns": inspection.video_presentation_offset_ns,
        "part_of_larger_session": null,
        "session_key": inspection.session_key,
        "car_type": inspection.car_type,
        "car_number": inspection.car_number,
        "car_class": inspection.car_class,
        "track_gps": inspection.track_gps.map(|(latitude, longitude)| json!({
            "latitude": latitude,
            "longitude": longitude,
        })),
        "track_name": inspection.track_name,
        "layout": inspection.layout,
        "track_length_m": inspection.track_length_m,
        "diagnostics": inspection
            .diagnostics
            .iter()
            .map(|diagnostic| {
                json!({
                    "severity": diagnostic.severity.name(),
                    "code": diagnostic.code,
                    "channel": diagnostic.channel,
                    "message": diagnostic.message,
                })
            })
            .collect::<Vec<_>>(),
    })
}

fn display_ids(values: &[i64]) -> String {
    if values.is_empty() {
        "unknown".into()
    } else {
        values
            .iter()
            .map(i64::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    }
}

fn display_strings(values: &[String]) -> String {
    if values.is_empty() {
        "unknown".into()
    } else {
        values.join(", ")
    }
}

fn display<T: std::fmt::Display>(value: Option<T>) -> String {
    value.map_or_else(|| "unknown".into(), |value| value.to_string())
}

fn format_duration(duration_ns: u64) -> String {
    let milliseconds = duration_ns / 1_000_000;
    let minutes = milliseconds / 60_000;
    let seconds = milliseconds / 1_000 % 60;
    let fraction = milliseconds % 1_000;
    format!("{minutes}:{seconds:02}.{fraction:03}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cli_arguments() {
        assert_eq!(
            arguments(["inspect".into(), "--json".into(), "run.ld".into()]),
            Ok(Command::Inspect {
                path: PathBuf::from("run.ld"),
                json: true,
                masks: Vec::new(),
            })
        );
        assert_eq!(
            arguments([
                "inspect".into(),
                "--mask".into(),
                "**/*.pds".into(),
                "-m".into(),
                "*.telemetry".into(),
                "logs".into(),
            ]),
            Ok(Command::Inspect {
                path: PathBuf::from("logs"),
                json: false,
                masks: vec!["**/*.pds".into(), "*.telemetry".into()],
            })
        );
        assert_eq!(
            arguments(["inspect".into(), "--help".into()]),
            Ok(Command::Help {
                topic: HelpTopic::Inspect
            })
        );
        assert_eq!(
            arguments(["help".into(), "convert".into()]),
            Ok(Command::Help {
                topic: HelpTopic::Convert
            })
        );
        assert_eq!(
            arguments(["convert".into(), "run.pds".into()]),
            Ok(Command::Convert {
                input: PathBuf::from("run.pds"),
                output: None,
                passes: PassMode::Apply,
            })
        );
        assert_eq!(
            arguments(["convert".into(), "--no-passes".into(), "run.pds".into()]),
            Ok(Command::Convert {
                input: PathBuf::from("run.pds"),
                output: None,
                passes: PassMode::Skip,
            })
        );
        assert_eq!(
            arguments([
                "convert".into(),
                "--strip-passes".into(),
                "run.telemetry".into()
            ]),
            Ok(Command::Convert {
                input: PathBuf::from("run.telemetry"),
                output: None,
                passes: PassMode::Strip,
            })
        );
        assert_eq!(
            arguments([
                "verify".into(),
                "a.telemetry".into(),
                "b.telemetry.jsonl".into()
            ]),
            Ok(Command::Verify {
                paths: vec![
                    PathBuf::from("a.telemetry"),
                    PathBuf::from("b.telemetry.jsonl"),
                ],
            })
        );
        assert!(arguments(Vec::<OsString>::new()).is_err());
        assert!(arguments(["inspect".into(), "one.ld".into(), "two.ld".into()]).is_err());
        assert!(arguments(["unknown".into()]).is_err());
        assert_eq!(
            arguments(["verify".into(), "--help".into()]),
            Ok(Command::Help {
                topic: HelpTopic::Verify
            })
        );
        assert!(arguments(["inspect".into(), "--mask".into()]).is_err());
    }

    #[test]
    fn glob_masks_match_relative_paths() {
        assert!(glob_match("**/*.pds", "sebring/a.pds"));
        assert!(glob_match("*.pds", "a.pds"));
        assert!(!glob_match("*.pds", "sebring/a.pds"));
        assert!(glob_match("sebring-2026/**", "sebring-2026/CT5/run.pds"));
        assert!(expand_braces("**/*.{ld,pds}")
            .iter()
            .any(|pattern| glob_match(pattern, "x.LD")));
        assert!(matches_any_mask(
            Path::new("/data"),
            Path::new("/data/sebring/run.pds"),
            &["*.pds".into()]
        ));
        assert!(!matches_any_mask(
            Path::new("/data"),
            Path::new("/data/sebring/run.pds"),
            &["**/*.ld".into()]
        ));
    }

    #[test]
    fn default_convert_target_is_native_telemetry() {
        let dest = default_telemetry_dest(Path::new("run.pds"));
        assert_eq!(dest, PathBuf::from("run.pds.telemetry"));
    }

    #[test]
    fn formats_lap_time() {
        assert_eq!(format_duration(83_456_789_000), "1:23.456");
    }
    #[test]
    fn converts_unix_days_and_source_dates() {
        let date = CivilDate {
            year: 2026,
            month: 8,
            day: 8,
        };
        assert_eq!(civil_from_days(days_from_civil(date)), Some(date));
        assert_eq!(parse_source_date("08/08/2026"), Some(date));
        assert_eq!(parse_source_date("2026-08-08"), Some(date));
    }

    #[test]
    fn rejects_a_stale_telemetry_clock_in_favor_of_creation_date() {
        let telemetry = CivilDate {
            year: 2022,
            month: 7,
            day: 1,
        };
        let created = CivilDate {
            year: 2026,
            month: 8,
            day: 8,
        };
        let selected = select_event_date(Some((telemetry, "gps_clock".into())), Some(created));
        assert_eq!(selected.selected, Some(created));
        assert_eq!(selected.source.as_deref(), Some("file_created_at"));
        assert!(selected.warning.unwrap().contains("rejected gps_clock"));
    }
}
