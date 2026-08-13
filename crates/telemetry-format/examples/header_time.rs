fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: header_time FILE.telemetry");
    let start = std::time::Instant::now();
    let valid = telemetry_format::read_valid_laps(&path).unwrap();
    let valid_us = start.elapsed();
    let start = std::time::Instant::now();
    let laps = telemetry_format::read_laps(&path).unwrap();
    let laps_us = start.elapsed();
    let start = std::time::Instant::now();
    let meta = telemetry_format::read_metadata(&path).unwrap();
    let meta_us = start.elapsed();
    println!("valid_laps={valid} in {valid_us:?}");
    println!("laps={} in {:?}", laps.len(), laps_us);
    println!(
        "metadata channels={} samples={} in {:?}",
        meta.channel_count, meta.sample_count, meta_us
    );
}
