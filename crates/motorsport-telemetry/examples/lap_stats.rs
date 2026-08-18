use motorsport_telemetry::{
    motorsport_telemetry_core::{can_convert, convert, TelemetrySource},
    open, SourceExt,
};

fn maximum_between(
    source: &dyn TelemetrySource,
    channel_index: usize,
    start_ns: u64,
    end_ns: u64,
) -> Option<f64> {
    let channel = source.channels().get(channel_index)?;
    let mut maximum: Option<f64> = None;

    for (chunk_index, chunk) in channel.chunks.iter().enumerate() {
        for local_index in 0..chunk.sample_count {
            let time_ns = source.sample_time_ns(channel_index, chunk_index, local_index);
            if time_ns < start_ns || time_ns >= end_ns {
                continue;
            }

            let value = source.decode(channel_index, chunk_index, local_index);
            if value.is_finite() {
                maximum = Some(maximum.map_or(value, |before| before.max(value)));
            }
        }
    }

    maximum
}

fn format_lap_time(duration_ns: u64) -> String {
    let total_ms = duration_ns / 1_000_000;
    let minutes = total_ms / 60_000;
    let seconds = total_ms % 60_000 / 1_000;
    let milliseconds = total_ms % 1_000;
    format!("{minutes}:{seconds:02}.{milliseconds:03}")
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::args_os()
        .nth(1)
        .ok_or("usage: lap_stats TELEMETRY_FILE")?;
    let file = open(path)?;
    let metadata = file.metadata();

    let speed_index = file
        .signal_roles()
        .speed
        .ok_or("no recognized speed channel")?;
    let speed_unit = &file.channels()[speed_index].unit;
    if !can_convert(speed_unit, "km/h") {
        return Err(format!("speed unit {speed_unit:?} cannot be converted to km/h").into());
    }

    // Include every sampled channel whose name mentions brakes and whose unit
    // is a pressure unit. This handles separate front/rear or master pressures.
    let brake_pressure_indices = file
        .channels()
        .iter()
        .enumerate()
        .filter(|(_, channel)| {
            channel.sample_count > 0
                && channel.name.to_ascii_lowercase().contains("brake")
                && can_convert(&channel.unit, "bar")
        })
        .map(|(index, _)| index)
        .collect::<Vec<_>>();
    if brake_pressure_indices.is_empty() {
        return Err("no brake-pressure channel with a recognized pressure unit".into());
    }

    let mut laps = metadata.laps.iter().filter(|lap| lap.complete).peekable();
    if laps.peek().is_none() {
        return Err("the recording contains no complete laps".into());
    }

    for lap in laps {
        let top_speed_kmh = maximum_between(&file, speed_index, lap.start_ns, lap.end_ns)
            .and_then(|value| convert(value, speed_unit, "km/h").ok());

        let max_brake_bar = brake_pressure_indices
            .iter()
            .filter_map(|&index| {
                maximum_between(&file, index, lap.start_ns, lap.end_ns)
                    .and_then(|value| convert(value, &file.channels()[index].unit, "bar").ok())
            })
            .reduce(f64::max);

        println!(
            "lap {:>3}: {}  top speed {:>7} km/h  max brake {:>7} bar",
            lap.number,
            format_lap_time(lap.duration_ns),
            top_speed_kmh.map_or_else(|| "n/a".into(), |value| format!("{value:.1}")),
            max_brake_bar.map_or_else(|| "n/a".into(), |value| format!("{value:.1}")),
        );
    }

    Ok(())
}
