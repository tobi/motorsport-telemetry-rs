//! Compare video sync in a converted `.telemetry` file with its original `AiM` MP4.

#![allow(
    missing_docs,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::print_stdout,
    clippy::print_stderr,
    clippy::unreadable_literal,
    clippy::float_cmp,
    clippy::format_push_string,
    reason = "test and example code: fail loudly, print freely, exact fixture values"
)]

use motorsport_telemetry::motorsport_telemetry_core::TelemetrySource;
use motorsport_telemetry::open;
use std::path::{Path, PathBuf};
use telemetry_format::write_telemetry;

fn main() {
    let src = PathBuf::from(
        std::env::args()
            .nth(1)
            .expect("usage: compare_aim MP4 [TELEMETRY]"),
    );
    let dest = std::env::args().nth(2).map_or_else(
        || {
            PathBuf::from("/tmp").join(format!(
                "{}.telemetry",
                src.file_stem().and_then(|s| s.to_str()).unwrap_or("aim")
            ))
        },
        PathBuf::from,
    );
    let report = compare(&src, &dest).unwrap_or_else(|err| {
        eprintln!("{err}");
        std::process::exit(1);
    });
    print!("{report}");
    if report.contains("MISMATCH") {
        std::process::exit(2);
    }
}

fn compare(src: &Path, dest: &Path) -> Result<String, Box<dyn std::error::Error>> {
    let mut out = String::new();
    let original = open(src)?;
    if dest.exists() {
        out.push_str(&format!("reusing {}\n", dest.display()));
    } else {
        out.push_str(&format!(
            "converting {} -> {}\n",
            src.display(),
            dest.display()
        ));
        write_telemetry(&original, dest)?;
    }
    let converted = open(dest)?;

    let src_frames = original.video_frame_count();
    let dst_frames = converted.video_frame_count();
    let src_offset = original.video_presentation_offset_ns();
    let dst_offset = converted.video_presentation_offset_ns();
    let src_times = original.video_presentation_times_ns().unwrap_or(&[]);
    let dst_times = converted.video_presentation_times_ns().unwrap_or(&[]);
    let duration = original
        .channels()
        .iter()
        .map(|channel| channel.duration_ns)
        .max()
        .unwrap_or(0);

    out.push_str(&format!(
        "source={} format={} duration_ms={:.3} channels={}\n",
        src.display(),
        original.format(),
        duration as f64 / 1e6,
        original.channels().len()
    ));
    out.push_str(&format!(
        "converted={} format={} size={}\n",
        dest.display(),
        converted.format(),
        std::fs::metadata(dest)?.len()
    ));
    out.push_str(&format!(
        "frames src={src_frames:?} dst={dst_frames:?} {}\n",
        ok(src_frames == dst_frames)
    ));
    out.push_str(&format!(
        "presentation_offset_ns src={src_offset:?} dst={dst_offset:?} {}\n",
        ok(src_offset == dst_offset)
    ));
    out.push_str(&format!(
        "pts_len src={} dst={} {}\n",
        src_times.len(),
        dst_times.len(),
        ok(src_times == dst_times)
    ));

    if src_times != dst_times {
        let first = src_times
            .iter()
            .zip(dst_times.iter())
            .position(|(a, b)| a != b);
        out.push_str(&format!(
            "MISMATCH first pts index={first:?} src_head={:?} dst_head={:?}\n",
            &src_times[..src_times.len().min(4)],
            &dst_times[..dst_times.len().min(4)]
        ));
    } else if let (Some(first), Some(last)) = (src_times.first(), src_times.last()) {
        out.push_str(&format!(
            "pts_range_ns {first} .. {last} ({:.3} ms)\n",
            (*last as f64 - *first as f64) / 1e6
        ));
    }

    let mut probes = vec![0u64];
    if duration > 0 {
        probes.push(duration.saturating_sub(1));
        probes.push(duration);
    }
    let mut t = 0u64;
    while t <= duration {
        probes.push(t);
        t = t.saturating_add(1_000_000_000);
        if t == 0 {
            break;
        }
    }
    t = 0;
    while t <= duration {
        probes.push(t);
        t = t.saturating_add(1_000_000);
        if t == 0 {
            break;
        }
    }
    if let Some(channel) = original
        .channels()
        .iter()
        .find(|channel| channel.sample_count > 0 && !channel.chunks.is_empty())
    {
        let chunk = &channel.chunks[0];
        for local in [
            0u64,
            channel.sample_count / 2,
            channel.sample_count.saturating_sub(1),
        ] {
            if local < channel.sample_count {
                probes.push(chunk.time_base_ns + local.saturating_mul(chunk.sample_period_ns));
            }
        }
    }
    probes.sort_unstable();
    probes.dedup();

    let mut frame_mismatches = 0usize;
    let mut ref_mismatches = 0usize;
    let mut first_frame = None;
    let mut first_ref = None;
    for &time in &probes {
        let src_frame = original.video_frame_at(time);
        let dst_frame = converted.video_frame_at(time);
        if src_frame != dst_frame {
            frame_mismatches += 1;
            if first_frame.is_none() {
                first_frame = Some((time, src_frame, dst_frame));
            }
        }
        let src_ref = original.video_reference_at(time);
        let dst_ref = converted.video_reference_at(time);
        if src_ref != dst_ref {
            ref_mismatches += 1;
            if first_ref.is_none() {
                first_ref = Some((time, src_ref, dst_ref));
            }
        }
    }

    out.push_str(&format!(
        "probes={} frame_mismatches={} ref_mismatches={} {}\n",
        probes.len(),
        frame_mismatches,
        ref_mismatches,
        ok(frame_mismatches == 0 && ref_mismatches == 0)
    ));
    if let Some((time, src_frame, dst_frame)) = first_frame {
        out.push_str(&format!(
            "MISMATCH first frame t_ns={time} t_ms={:.3} src={src_frame:?} dst={dst_frame:?}\n",
            time as f64 / 1e6
        ));
    }
    if let Some((time, src_ref, dst_ref)) = first_ref {
        out.push_str(&format!(
            "MISMATCH first video_reference t_ns={time} t_ms={:.3} src={src_ref:?} dst={dst_ref:?}\n",
            time as f64 / 1e6
        ));
    }

    out.push_str(&format!(
        "channels src={} dst={} (JSONL keeps aligned channels)\n",
        original.channels().len(),
        converted.channels().len()
    ));

    for &time in &[0u64, 1_000_000, 1_000_000_000, duration / 2] {
        if time > duration && time != 0 {
            continue;
        }
        out.push_str(&format!(
            "sample t_ms={:.3} frame={:?} pts={:?} ref={:?}\n",
            time as f64 / 1e6,
            converted.video_frame_at(time),
            converted.video_presentation_time_ns(time),
            converted.video_reference_at(time)
        ));
    }
    Ok(out)
}

fn ok(same: bool) -> &'static str {
    if same {
        "OK"
    } else {
        "MISMATCH"
    }
}
