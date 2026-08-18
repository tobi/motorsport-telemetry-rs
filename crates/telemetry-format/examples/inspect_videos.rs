use motorsport_telemetry_core::TelemetrySource;
use telemetry_format::NativeRecording;

fn main() {
    let path = std::env::args().nth(1).expect("file");
    let file = NativeRecording::open(&path).unwrap();
    let meta = file.metadata();
    println!(
        "format={} duration_s={:.1} channels={} samples={}",
        file.format(),
        meta.duration_ns as f64 / 1e9,
        meta.channel_count,
        meta.sample_count
    );
    println!("laps={} valid_laps={}", meta.laps.len(), meta.valid_laps);
    println!("videos={}", meta.videos.len());
    for v in &meta.videos {
        println!(
            "  index={} file={} blake3={} frames={}",
            v.index,
            v.filename,
            v.blake3.is_some(),
            v.frame_count
        );
    }
    let avi = file.channels().iter().position(|c| {
        let n = c.name.replace([' ', '_', '-'], "").to_ascii_lowercase();
        n == "avifileindex"
    });
    let sync = file.channels().iter().position(|c| {
        let n = c.name.replace([' ', '_', '-'], "").to_ascii_lowercase();
        matches!(n.as_str(), "avisynctime" | "avitime")
    });
    println!("avifileindex ch={avi:?} avisynctime ch={sync:?}");
    if let (Some(avi), Some(sync)) = (avi, sync) {
        let last = file.channels()[avi].sample_count.saturating_sub(1);
        let mid = 91448u64; // first row of file 2
        for t_idx in [0u64, 91447, mid.min(last), last] {
            let t = file.sample_time_ns(avi, 0, t_idx);
            let idx = file.decode(avi, 0, t_idx);
            let syn = file.decode(sync, 0, t_idx);
            let refer = file.video_reference_at(t);
            println!("row {t_idx} t={t} avifile={idx} avitime={syn} ref={refer:?}");
        }
    }
    // use facade? just print channel names for core
    for want in [
        "Throttle Pedal",
        "Steering Angle",
        "Vehicle Speed",
        "Gear",
        "Lap Number",
        "DriverID",
    ] {
        let hit = file.channels().iter().any(|c| c.name == want);
        println!("channel {want}: {hit}");
    }
}
