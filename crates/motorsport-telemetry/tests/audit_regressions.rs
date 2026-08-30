//! Minimal files reproducing naming/layout combinations observed in the NAS
//! audit. No real recording is required by these tests.
use motorsport_telemetry::motorsport_telemetry_core::TelemetrySource;
use motorsport_telemetry::{open, SourceExt};

/// Third-party LD export: physical units occupy the short-name slot, while
/// both throttle plate and driver demand, and both pressure and pedal, exist.
fn pedal_export() -> Vec<u8> {
    let channels = [
        ("Corr Speed", "m/s", 50.0_f32),
        ("Throttle Pos", "ratio", 0.9),
        ("Driver Throttle Pos", "ratio", 0.4),
        ("P_F_BRAKE", "bar", 30.0),
        ("Brake Pos", "ratio", 0.3),
        ("Steering Angle", "deg", -15.0),
    ];
    let table = 0x200;
    let payload = table + channels.len() * 124;
    let mut bytes = vec![0u8; payload + channels.len() * 8];
    let put32 =
        |data: &mut [u8], at: usize, v: u32| data[at..at + 4].copy_from_slice(&v.to_le_bytes());
    put32(&mut bytes, 0, 0x40);
    put32(&mut bytes, 8, table as u32);
    for (i, (name, unit, value)) in channels.iter().enumerate() {
        let at = table + i * 124;
        if i + 1 < channels.len() {
            put32(&mut bytes, at + 4, (at + 124) as u32);
        }
        put32(&mut bytes, at + 8, (payload + i * 8) as u32);
        put32(&mut bytes, at + 12, 2);
        for (offset, value) in [(0x12, 7_u16), (0x14, 4), (0x16, 10), (0x1a, 1), (0x1c, 1)] {
            bytes[at + offset..at + offset + 2].copy_from_slice(&value.to_le_bytes());
        }
        bytes[at + 0x20..at + 0x20 + name.len()].copy_from_slice(name.as_bytes());
        bytes[at + 0x40..at + 0x40 + unit.len()].copy_from_slice(unit.as_bytes());
        for sample in 0..2 {
            let position = payload + i * 8 + sample * 4;
            bytes[position..position + 4].copy_from_slice(&value.to_le_bytes());
        }
    }
    bytes
}

#[test]
fn synthetic_ld_pedals_normalize_and_survive_native_conversion() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("pedals.ld");
    std::fs::write(&input, pedal_export()).unwrap();
    let source = open(&input).unwrap();
    let destination = directory.path().join("pedals.telemetry");
    telemetry_format::write_from_source(&source, &destination).unwrap();
    for file in [source, open(&destination).unwrap()] {
        let roles = file.signal_roles();
        assert_eq!(
            file.channels()[roles.throttle.unwrap()].name,
            "Driver Throttle Pos"
        );
        assert_eq!(file.channels()[roles.brake.unwrap()].name, "Brake Pos");
        let sample = file.normalizer().sample(50_000_000);
        assert_eq!(sample.speed_mps, Some(50.0));
        assert!((sample.throttle_fraction.unwrap() - 0.4).abs() < 1e-6);
        assert!((sample.brake_fraction.unwrap() - 0.3).abs() < 1e-6);
        assert!((sample.steering_deg.unwrap() + 15.0).abs() < 1e-12);
    }
}
