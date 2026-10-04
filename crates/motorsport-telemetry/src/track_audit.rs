//! Read-only physical and state audits, separate from reader normalization.
use motorsport_telemetry_core::{
    names,
    track::{trusted_position, TrackGeometry, TrackPosition},
    LapBoundary, LapKind, TelemetrySource,
};
use serde::Serialize;

/// Track-specific audit parameters. Slow laps are findings, never invented FCY.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackAuditOptions {
    /// Atlas slug; otherwise matched from valid native GPS or native venue.
    pub track: Option<String>,
    /// Layout identifier; otherwise the facility's first layout.
    pub layout: Option<String>,
    /// Optional minimum flying lap duration, in seconds.
    pub min_lap_s: Option<f64>,
    /// Optional review threshold for slow flying laps, in seconds.
    pub max_lap_s: Option<f64>,
    /// Physical speed ceiling used for GPS discontinuity checks, in m/s.
    pub max_speed_mps: f64,
    /// Allowed centerline distance in metres for circuit traversal checks.
    pub corridor_m: f64,
}

impl Default for TrackAuditOptions {
    fn default() -> Self {
        Self {
            track: None,
            layout: None,
            min_lap_s: None,
            max_lap_s: None,
            max_speed_mps: 120.0,
            corridor_m: 30.0,
        }
    }
}

/// One audit finding, with a stable code for corpus aggregation.
#[derive(Debug, Serialize)]
pub struct TrackFinding {
    /// `error`, `review`, or `info`. Missing evidence never passes as verified.
    pub severity: &'static str,
    /// Stable finding identifier.
    pub code: &'static str,
    /// File-relative interval number, when applicable.
    pub lap: Option<i64>,
    /// File-relative evidence time, when applicable.
    pub time_ns: Option<u64>,
    /// Explanation for a human reviewer.
    pub message: String,
}

/// Machine-readable lap and GPS audit result.
#[derive(Debug, Serialize)]
pub struct TrackAuditReport {
    /// Atlas facility slug, when identified.
    pub track: Option<String>,
    /// Selected atlas layout identifier.
    pub layout: Option<String>,
    /// Applied minimum, including the atlas length / speed physical bound.
    pub min_lap_s: Option<f64>,
    /// User-specified slow-lap review threshold.
    pub max_lap_s: Option<f64>,
    /// Applied physical speed ceiling.
    pub max_speed_mps: f64,
    /// Applied centerline corridor width.
    pub corridor_m: f64,
    /// Number of native position samples inspected.
    pub gps_samples: u64,
    /// Position samples with valid receiver status and reported accuracy.
    pub trusted_gps_samples: u64,
    /// Number of interval boundaries inspected with nearby trusted GPS.
    pub gps_checked_boundaries: u64,
    /// Findings, including missing signals and uncertain activity.
    pub findings: Vec<TrackFinding>,
}

impl TrackAuditReport {
    /// True when the audit found a violation rather than an evidence limit.
    pub fn has_errors(&self) -> bool {
        self.findings.iter().any(|f| f.severity == "error")
    }
    fn add(
        &mut self,
        severity: &'static str,
        code: &'static str,
        lap: Option<i64>,
        time_ns: Option<u64>,
        message: impl Into<String>,
    ) {
        self.findings.push(TrackFinding {
            severity,
            code,
            lap,
            time_ns,
            message: message.into(),
        });
    }
}

// Observe ordered circuit quarters away from the start/finish and parallel
// pit lane. Two tours inside an out fragment expose missing activity even
// when a slow pit-lane pass correctly supplies no flying-lap beacon.
#[derive(Default)]
struct CircuitTours {
    expected: u8,
    completed: u32,
    previous_time: Option<u64>,
}

impl CircuitTours {
    fn observe(&mut self, time: u64, position: Option<TrackPosition>) {
        if self
            .previous_time
            .is_some_and(|before| time - before > 2_000_000_000)
        {
            self.expected = 0;
        }
        self.previous_time = Some(time);
        let Some(p) = position else {
            self.expected = 0;
            return;
        };
        if p.pit_sector {
            return;
        }
        let quarter = ((p.progress * 4.0) as u8).min(3);
        match (self.expected, quarter) {
            (0, 1) => self.expected = 2,
            (2, 2) => self.expected = 3,
            (3, 3) => {
                self.completed += 1;
                self.expected = 0;
            }
            (2, 0 | 3) | (3, 0 | 1) => self.expected = 0,
            _ => {}
        }
    }
}

/// Audits physical plausibility without modifying source samples or metadata.
pub fn audit_track(
    source: &dyn TelemetrySource,
    options: &TrackAuditOptions,
) -> Result<TrackAuditReport, String> {
    for (name, value) in [
        ("max speed", options.max_speed_mps),
        ("corridor", options.corridor_m),
    ] {
        if !value.is_finite() || value <= 0.0 {
            return Err(format!("{name} must be finite and positive"));
        }
    }
    for value in [options.min_lap_s, options.max_lap_s].into_iter().flatten() {
        if !value.is_finite() || value <= 0.0 {
            return Err("lap limits must be finite and positive".into());
        }
    }
    if options
        .min_lap_s
        .zip(options.max_lap_s)
        .is_some_and(|(a, b)| a > b)
    {
        return Err("minimum lap exceeds maximum lap".into());
    }
    let metadata = source.metadata();
    let latitude = names::find(source.channels(), &["gpslatitude", "latitude"]);
    let mut fixes = Vec::new();
    let mut gps_samples = 0;
    if let Some(index) = latitude {
        for (chunk_i, chunk) in source.channels()[index].chunks.iter().enumerate() {
            for local in 0..chunk.sample_count {
                let time = source.sample_time_ns(index, chunk_i, local);
                gps_samples += 1;
                if let Some((lat, lon)) = trusted_position(source, time) {
                    fixes.push((time, lat, lon));
                }
            }
        }
    }
    let track = if let Some(slug) = &options.track {
        Some(
            motorsport_track_atlas::find_track(slug)
                .ok_or_else(|| format!("unknown atlas track: {slug}"))?,
        )
    } else {
        fixes
            .first()
            .and_then(|(_, a, b)| {
                motorsport_track_atlas::match_track(*a, *b, 10_000.0).map(|m| m.track)
            })
            .or_else(|| {
                motorsport_track_atlas::find_track_for_venue(&metadata.source_identity.venue)
            })
    };
    let layout = match (track, options.layout.as_deref()) {
        (Some(t), Some(id)) => Some(
            t.layouts
                .iter()
                .find(|l| l.id == id)
                .ok_or_else(|| format!("unknown layout {id} for {}", t.slug))?,
        ),
        (Some(t), None) => t.layouts.first(),
        (None, Some(_)) => return Err("a layout requires an identified track".into()),
        (None, None) => None,
    };
    let minimum = options.min_lap_s.or_else(|| {
        layout
            .and_then(|l| l.length_m)
            .map(|d| d / options.max_speed_mps)
    });
    let geometry = layout.and_then(TrackGeometry::new);
    let mut report = TrackAuditReport {
        track: track.map(|t| t.slug.into()),
        layout: layout.map(|l| l.id.into()),
        min_lap_s: minimum,
        max_lap_s: options.max_lap_s,
        max_speed_mps: options.max_speed_mps,
        corridor_m: options.corridor_m,
        gps_samples,
        trusted_gps_samples: fixes.len() as u64,
        gps_checked_boundaries: 0,
        findings: Vec::new(),
    };
    if track.is_none() {
        report.add(
            "review",
            "missing-track",
            None,
            None,
            "No atlas track identified; supply --track.",
        );
    }
    if fixes.is_empty() {
        report.add("review","missing-trusted-gps",None,None,"No native GPS with usable fix/accuracy status; physical track/pit boundaries cannot be certified.");
    }
    if !source
        .channels()
        .iter()
        .filter(|c| c.sample_count > 0)
        .any(|c| {
            ["lapnumber", "lapnum", "lapcount", "beaconeventcount"]
                .iter()
                .any(|n| names::eq(&c.name, n))
        })
    {
        report.add(
            "review",
            "missing-counter",
            None,
            None,
            "No recognized native lap counter.",
        );
    }
    if !source
        .channels()
        .iter()
        .filter(|c| c.sample_count > 0)
        .any(|c| {
            ["currentlaptime", "laptime", "laptimerunning", "lapprogress"]
                .iter()
                .any(|n| names::eq(&c.name, n))
        })
    {
        report.add(
            "review",
            "missing-timer",
            None,
            None,
            "No recognized running lap timer/progress signal.",
        );
    }
    if !source
        .channels()
        .iter()
        .filter(|c| c.sample_count > 0)
        .any(|c| {
            [
                "groundspeed",
                "speedref",
                "speedwspdapp",
                "vehiclespeed",
                "gpsspeed",
                "speed",
            ]
            .iter()
            .any(|n| names::eq(&c.name, n))
        })
    {
        report.add(
            "review",
            "missing-speed",
            None,
            None,
            "No recognized motion channel; stopped versus moving activity cannot be checked.",
        );
    }
    let mut discontinuities = 0;
    let mut off_track = 0;
    let gps_speed = names::find(source.channels(), &["gpsspeed"]);
    if let Some(g) = &geometry {
        for &(_, lat, lon) in &fixes {
            if g.locate(lat, lon).is_some_and(|p| p.distance_m > 100.0) {
                off_track += 1;
            }
        }
        for pair in fixes.windows(2) {
            let dt = (pair[1].0 - pair[0].0) as f64 / 1e9;
            let dx = (pair[1].2 - pair[0].2) * pair[0].1.to_radians().cos() * 111_195.0;
            let dy = (pair[1].1 - pair[0].1) * 111_195.0;
            if dt > 0.0 && dt <= 1.0 && dx.hypot(dy) > options.max_speed_mps * dt + 16.0 {
                discontinuities += 1;
            }
        }
        if off_track > 0 {
            report.add("review","gps-away-from-circuit",None,None,format!("{off_track} trusted-status samples exceed 100 m from atlas centerline; inspect drift/off-track activity before using them as lap evidence."));
        }
        if discontinuities > 0 {
            report.add("review","gps-discontinuity",None,None,format!("{discontinuities} position jumps exceed the speed ceiling plus 16 m accuracy allowance."));
        }
    }
    let (native_laps, observations) = motorsport_telemetry_core::inspect_lap_recovery(source);
    for observation in observations {
        let message = match observation.code {
            "recovered-timer-dropout" => "Timer resumes its previous trajectory; the transient reset was rejected as a crossing.",
            "recovered-counter-dropout" => "Counter drop recovers within the confirmation window; the transient value was ignored.",
            "recovered-pit-close-beacon" => "Counter increment before a stationary reset was merged into the in fragment, avoiding a false short in-lap.",
            "ignored-counter-rearm" => "Uncorroborated 0→1 arming was ignored; it cannot leave pit/stopped activity or create a crossing.",
            "ignored-pit-lane-beacon" => "Slow counter increment in the parallel GPS pit lane was ignored as a circuit crossing.",
            "recovered-first-crossing" => "Initial 0→1 counter increment was independently corroborated as a circuit crossing.",
            _ => "Native source event was recovered during lap normalization.",
        };
        report.add(
            "info",
            observation.code,
            None,
            Some(observation.time_ns),
            message,
        );
    }
    // Only compare annotations when a recognized native lap signal exists.
    // Authoritative vendor annotations can differ legitimately, so request
    // review instead of replacing them or declaring the recovery infallible.
    if source.source_lap_metadata().is_some() && !native_laps.is_empty() {
        let shape =
            |l: &motorsport_telemetry_core::LapMetadata| (l.start_ns, l.end_ns, l.kind, l.complete);
        if native_laps.iter().map(shape).collect::<Vec<_>>()
            != metadata.laps.iter().map(shape).collect::<Vec<_>>()
        {
            report.add("review", "native-lap-disagreement", None, None,
                "Stored lap annotations disagree with independent native counter/timer/motion/GPS recovery; inspect possible stale parsing or intentional vendor annotations.");
        }
    }
    let laps = &metadata.laps;
    // Inspect persisted transitions before classification can repair them.
    let stored_laps = source.source_lap_metadata();
    for pair in stored_laps
        .as_ref()
        .map_or(laps.as_slice(), |m| m.laps.as_slice())
        .windows(2)
    {
        if matches!(pair[0].kind, LapKind::Pit | LapKind::Stopped)
            && pair[1].kind == LapKind::Flying
        {
            report.add("error", "impossible-transition", Some(pair[1].number), Some(pair[1].start_ns),
                "Pit/stopped activity transitions directly to a flying lap without an out fragment.");
        }
    }

    // A GPS wrap far from every dash boundary suggests an omitted beacon.
    // Subsample by native time rather than assuming a receiver cadence.
    if let Some(g) = &geometry {
        let mut previous: Option<(u64, f64)> = None;
        let mut last_probe = None;
        let mut last_finding = None;
        for &(time, a, b) in &fixes {
            if last_probe.is_some_and(|t| time - t < 500_000_000) {
                continue;
            }
            last_probe = Some(time);
            let Some(position) = g
                .locate(a, b)
                .filter(|p| p.distance_m <= options.corridor_m)
            else {
                previous = None;
                continue;
            };
            if let Some((before, progress)) = previous {
                if time - before <= 2_000_000_000
                    && progress > 0.9
                    && position.progress < 0.1
                    && gps_speed.is_some_and(|i| {
                        source
                            .sample_at(i, time, false)
                            .and_then(|v| {
                                motorsport_telemetry_core::convert(
                                    v,
                                    &source.channels()[i].unit,
                                    "m/s",
                                )
                                .ok()
                            })
                            .is_some_and(|v| v.is_finite() && v > 25.0)
                    })
                    && !laps.iter().any(|l| {
                        l.start_ns.abs_diff(time) <= 5_000_000_000
                            || l.end_ns.abs_diff(time) <= 5_000_000_000
                    })
                    && last_finding.is_none_or(|t| time - t > 10_000_000_000)
                {
                    report.add("review","possible-missed-crossing",None,Some(time),"Native GPS crosses atlas start/finish with no lap boundary within 5 s; inspect a missing dash event or GPS drift.");
                    last_finding = Some(time);
                }
            }
            previous = Some((time, position.progress));
        }
    }
    for (i, lap) in laps.iter().enumerate() {
        let duration = lap.duration_ns as f64 / 1e9;
        for (boundary, code, message) in [
            (LapBoundary::GpsPitEntry, "recovered-moving-pit-pass", "Separate moving pit lane recovered from native GPS despite no dash reset; atlas marker times are estimates."),
            (LapBoundary::MotionDeparture, "recovered-motion-departure", "Stationary activity separated from the out fragment using sustained motion and later circuit evidence."),
            (LapBoundary::RejectedCrossing, "rejected-short-crossing", "Implausibly short crossing rejected against the native reference; activity remains uncertain."),
        ] {
            if lap.end_boundary == boundary {
                report.add("info", code, Some(lap.number), Some(lap.end_ns), message);
            }
        }
        if lap.kind == LapKind::Stopped {
            report.add("review", "stopped-on-circuit", Some(lap.number), Some(lap.start_ns), "GPS locates sustained standstill on the circuit; recording termination may indicate a shutdown, and does not establish pit activity.");
        }

        if lap.end_ns <= lap.start_ns
            || lap.end_ns > metadata.duration_ns
            || lap.duration_ns != lap.end_ns.saturating_sub(lap.start_ns)
        {
            report.add(
                "error",
                "invalid-interval",
                Some(lap.number),
                Some(lap.start_ns),
                "Non-positive, inconsistent or out-of-recording interval.",
            );
        }
        if lap.number != i as i64 + 1 || lap.stint == 0 || lap.kind == LapKind::Unknown {
            report.add(
                "error",
                "invalid-lap-state",
                Some(lap.number),
                Some(lap.start_ns),
                "Invalid virtual numbering, stint, or unresolved kind.",
            );
        }
        if i > 0 {
            let previous = &laps[i - 1];
            if previous.end_ns > lap.start_ns || lap.stint < previous.stint {
                report.add(
                    "error",
                    "interval-order",
                    Some(lap.number),
                    Some(lap.start_ns),
                    "Intervals overlap or stints decrease.",
                );
            }
        }
        if lap.kind == LapKind::Flying {
            let invalid_boundary = matches!(
                lap.start_boundary,
                LapBoundary::CounterReset
                    | LapBoundary::Stationary
                    | LapBoundary::MotionDeparture
                    | LapBoundary::GpsPitEntry
                    | LapBoundary::GpsPitExit
                    | LapBoundary::RejectedCrossing
                    | LapBoundary::RecordingEdge
            ) || matches!(
                lap.end_boundary,
                LapBoundary::CounterReset
                    | LapBoundary::GpsPitEntry
                    | LapBoundary::GpsPitExit
                    | LapBoundary::RejectedCrossing
                    | LapBoundary::RecordingEdge
            );
            if !lap.complete || invalid_boundary {
                report.add(
                    "error",
                    "flying-without-crossings",
                    Some(lap.number),
                    Some(lap.start_ns),
                    "Flying lap lacks two eligible crossing boundaries.",
                );
            }
            if minimum.is_some_and(|min| duration < min) {
                report.add(
                    "error",
                    "short-flying-lap",
                    Some(lap.number),
                    Some(lap.start_ns),
                    format!(
                        "{duration:.3} s is below the configured {:.3} s minimum.",
                        minimum.unwrap_or(0.0)
                    ),
                );
            }
            if options.max_lap_s.is_some_and(|max| duration > max) {
                report.add("review","long-flying-lap",Some(lap.number),Some(lap.start_ns),format!("{duration:.3} s exceeds the review threshold; FCY, stoppage or missing crossings need evidence."));
            }
        }
        if lap.kind == LapKind::Uncertain {
            report.add(
                "review",
                "uncertain-activity",
                Some(lap.number),
                Some(lap.start_ns),
                "Activity remains uncertain; do not count it as a verified lap or pit interval.",
            );
        }
        if lap.kind == LapKind::In
            && lap.end_boundary == LapBoundary::CounterReset
            && minimum.is_some_and(|m| duration < m * 0.5)
        {
            report.add("review","short-in-lap",Some(lap.number),Some(lap.start_ns),"Very short reset-bounded in-lap; check whether the preceding beacon occurred in the pit lane.");
        }
        if let Some(g) = &geometry {
            for time in [lap.start_ns, lap.end_ns] {
                if let Some((a, b)) = trusted_position(source, time) {
                    report.gps_checked_boundaries += 1;
                    if lap.kind == LapKind::Pit
                        // The exclusive end is the observed return to the
                        // circuit. It is expected to lie outside pit sector.
                        && !(time == lap.end_ns && lap.end_boundary == LapBoundary::GpsPitExit)
                        && g.locate(a, b)
                            .is_some_and(|p| p.distance_m <= 20.0 && !p.pit_sector)
                    {
                        report.add("error","pit-on-circuit",Some(lap.number),Some(time),"Pit-labelled activity has GPS on the circuit outside the pit sector; a stopped/crashed car must not become pit time.");
                    }
                }
            }
            if matches!(lap.kind, LapKind::Flying | LapKind::Out | LapKind::OutIn) {
                let mut mask = 0u8;
                let mut observed = 0;
                let mut probes = 0;
                let mut tours = CircuitTours::default();
                let stride = lap.duration_ns.div_ceil(4096).max(1_000_000_000);
                for bin in 0..lap.duration_ns.div_ceil(stride) {
                    let time = lap.start_ns.saturating_add(bin.saturating_mul(stride));
                    probes += 1;
                    let position = trusted_position(source, time).and_then(|(a, b)| {
                        g.locate(a, b)
                            .filter(|p| p.distance_m <= options.corridor_m)
                    });
                    tours.observe(time, position);
                    if let Some(p) = position {
                        observed += 1;
                        mask |= 1 << ((p.progress * 4.0) as u8).min(3);
                    }
                }
                if lap.kind == LapKind::Flying
                    && observed >= 20
                    && observed * 4 >= probes * 3
                    && mask != 15
                {
                    report.add(
                        "error",
                        "incomplete-circuit",
                        Some(lap.number),
                        Some(lap.start_ns),
                        "GPS does not traverse all four circuit quarters during a flying interval.",
                    );
                }
                if matches!(lap.kind, LapKind::Out | LapKind::OutIn) && tours.completed >= 2 {
                    report.add("review", "possible-merged-activity", Some(lap.number), Some(lap.start_ns), format!("Native GPS traverses circuit quarters 1–3 in order {} times inside one out fragment; inspect an unrecorded pit pass or missed beacon. The dash alone cannot delimit this activity.", tours.completed));
                }
            }
        }
    }
    if metadata.valid_laps as usize
        != laps
            .iter()
            .filter(|l| l.kind == LapKind::Flying && l.complete)
            .count()
    {
        report.add(
            "error",
            "valid-lap-count",
            None,
            None,
            "Stored valid-lap count disagrees with complete flying intervals.",
        );
    }
    if metadata
        .fastest_lap
        .as_ref()
        .is_some_and(|f| f.kind != LapKind::Flying || !f.complete || !laps.contains(f))
    {
        report.add(
            "error",
            "invalid-fastest",
            None,
            None,
            "Fastest lap is not a returned complete flying interval.",
        );
    }
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use motorsport_telemetry_core::{
        Channel, Chunk, LapMetadata, SampleType, SourceLapMetadata, UnitSource,
    };

    #[test]
    fn circuit_tours_require_ordered_motion_and_contiguous_evidence() {
        let position = |progress| {
            Some(TrackPosition {
                progress,
                distance_m: 0.0,
                signed_distance_m: 0.0,
                pit_sector: false,
            })
        };
        let count = |progress: &[f64], period: u64| {
            let mut tours = CircuitTours::default();
            for (i, &p) in progress.iter().enumerate() {
                tours.observe(i as u64 * period, position(p));
            }
            tours.completed
        };
        assert_eq!(
            count(
                &[0.3, 0.4, 0.6, 0.8, 0.9, 0.0, 0.3, 0.6, 0.8],
                1_000_000_000
            ),
            2
        );
        assert_eq!(count(&[0.8, 0.6, 0.3, 0.8, 0.6, 0.3], 1_000_000_000), 0);
        assert_eq!(count(&[0.3, 0.6, 0.8], 3_000_000_000), 0);
        assert_eq!(count(&[0.3, 0.3, 0.3, 0.3], 1_000_000_000), 0);
        let mut tours = CircuitTours::default();
        tours.observe(0, position(0.3));
        tours.observe(1_000_000_000, None);
        tours.observe(2_000_000_000, position(0.6));
        tours.observe(3_000_000_000, position(0.8));
        assert_eq!(tours.completed, 0);
    }
    struct Stored {
        channels: Vec<Channel>,
        laps: Vec<LapMetadata>,
    }
    impl TelemetrySource for Stored {
        fn path(&self) -> &'static str {
            "stored"
        }
        fn format(&self) -> &'static str {
            "stored"
        }
        fn channels(&self) -> &[Channel] {
            &self.channels
        }
        fn decode(&self, _: usize, _: usize, _: u64) -> f64 {
            0.0
        }
        fn source_lap_metadata(&self) -> Option<SourceLapMetadata> {
            Some(SourceLapMetadata {
                laps: self.laps.clone(),
                fastest_lap: None,
            })
        }
    }

    #[test]
    fn atlas_audit_flags_two_tours_inside_an_out_fragment() {
        struct GpsRecording {
            stored: Stored,
            values: Vec<Vec<f64>>,
        }
        impl TelemetrySource for GpsRecording {
            fn path(&self) -> &'static str {
                "merged-out"
            }
            fn format(&self) -> &'static str {
                "stored"
            }
            fn channels(&self) -> &[Channel] {
                &self.stored.channels
            }
            fn decode(&self, channel: usize, _: usize, local: u64) -> f64 {
                self.values[channel][local as usize]
            }
            fn source_lap_metadata(&self) -> Option<SourceLapMetadata> {
                self.stored.source_lap_metadata()
            }
        }
        let track = motorsport_track_atlas::find_track("road-atlanta").unwrap();
        let geo: serde_json::Value =
            serde_json::from_str(track.layouts[0].centerline_geojson).unwrap();
        let line = geo["features"]
            .as_array()
            .unwrap()
            .iter()
            .find(|f| f["properties"]["role"] == "outline")
            .unwrap()["geometry"]["coordinates"]
            .as_array()
            .unwrap();
        for tours in [1, 2] {
            let duration = tours * 60;
            let mut values = vec![Vec::new(); 5];
            for second in 0..=duration {
                let index = second % 60 * (line.len() - 1) / 60;
                values[0].push(line[index][1].as_f64().unwrap());
                values[1].push(line[index][0].as_f64().unwrap());
                values[2].push(3.0);
                values[3].push(1.0);
                values[4].push(70.0);
            }
            let channels = [
                ("GPS Latitude", "deg"),
                ("GPS Longitude", "deg"),
                ("GPS Fix Type", "raw"),
                ("GPS Position Accuracy", "m"),
                ("GPS Speed", "m/s"),
            ]
            .into_iter()
            .enumerate()
            .map(|(i, (name, unit))| Channel {
                id: i as u32,
                name: name.into(),
                unit: unit.into(),
                unit_source: UnitSource::Declared,
                sample_type: SampleType::F64,
                sample_count: duration as u64 + 1,
                duration_ns: duration as u64 * 1_000_000_000,
                chunks: vec![Chunk {
                    time_base_ns: 0,
                    sample_base: 0,
                    data_ptr: 0,
                    sample_count: duration as u64 + 1,
                    sample_period_ns: 1_000_000_000,
                }],
            })
            .collect();
            let mut lap = LapMetadata::interval(1, 0, duration as u64 * 1_000_000_000, false);
            lap.kind = LapKind::Out;
            lap.stint = 1;
            let source = GpsRecording {
                stored: Stored {
                    channels,
                    laps: vec![lap],
                },
                values,
            };
            let audit = audit_track(
                &source,
                &TrackAuditOptions {
                    track: Some("road-atlanta".into()),
                    ..Default::default()
                },
            )
            .unwrap();
            assert_eq!(audit.trusted_gps_samples, duration as u64);
            assert_eq!(
                audit
                    .findings
                    .iter()
                    .any(|f| f.code == "possible-merged-activity"),
                tours == 2,
                "{audit:?}"
            );
        }
    }
    #[test]
    fn persisted_pit_to_flying_and_reset_anchored_laps_fail_independent_audit() {
        let mut pit = LapMetadata::interval(1, 0, 20_000_000_000, false);
        pit.kind = LapKind::Pit;
        pit.stint = 1;
        let mut flying = LapMetadata::interval(2, 20_000_000_000, 80_000_000_000, true);
        flying.kind = LapKind::Flying;
        flying.stint = 1;
        flying.start_boundary = LapBoundary::CounterReset;
        flying.end_boundary = LapBoundary::CounterCrossing;
        let source = Stored {
            laps: vec![pit, flying],
            channels: vec![Channel {
                id: 0,
                name: "Clock".into(),
                unit: "s".into(),
                unit_source: UnitSource::Declared,
                sample_type: SampleType::F64,
                sample_count: 100,
                duration_ns: 100_000_000_000,
                chunks: vec![Chunk {
                    time_base_ns: 0,
                    sample_base: 0,
                    data_ptr: 0,
                    sample_count: 100,
                    sample_period_ns: 1_000_000_000,
                }],
            }],
        };
        let report = audit_track(&source, &TrackAuditOptions::default()).unwrap();
        assert!(report.has_errors());
        for code in ["impossible-transition", "flying-without-crossings"] {
            assert!(
                report
                    .findings
                    .iter()
                    .any(|f| f.code == code && f.severity == "error"),
                "{report:?}"
            );
        }
        assert!(report
            .findings
            .iter()
            .any(|f| f.code == "missing-trusted-gps"));
    }
    #[test]
    fn historical_short_uncertain_and_missing_out_patterns_are_flagged() {
        // SCHD0301's 22 s flying candidate, DHH's 11 s in fragment,
        // FP1's unresolved opening, and SCHD0304's pit-to-flying sequence.
        for (kind, duration, code) in [
            (LapKind::Flying, 22, "short-flying-lap"),
            (LapKind::In, 11, "short-in-lap"),
            (LapKind::Uncertain, 68, "uncertain-activity"),
        ] {
            let mut lap =
                LapMetadata::interval(1, 0, duration * 1_000_000_000, kind == LapKind::Flying);
            lap.kind = kind;
            lap.stint = 1;
            lap.start_boundary = LapBoundary::CounterCrossing;
            lap.end_boundary = if kind == LapKind::In {
                LapBoundary::CounterReset
            } else {
                LapBoundary::CounterCrossing
            };
            let mut source = stored_with_duration(vec![lap], 400);
            // No native evidence: review remains explicit rather than certifying.
            let report = audit_track(
                &source,
                &TrackAuditOptions {
                    min_lap_s: Some(72.0),
                    ..Default::default()
                },
            )
            .unwrap();
            assert!(report.findings.iter().any(|f| f.code == code), "{report:?}");
            source.laps[0].end_boundary = LapBoundary::RejectedCrossing;
            let report = audit_track(&source, &TrackAuditOptions::default()).unwrap();
            assert!(report
                .findings
                .iter()
                .any(|f| f.code == "rejected-short-crossing"));
        }
        for before in [LapKind::Pit, LapKind::Stopped] {
            let mut stopped = LapMetadata::interval(1, 0, 20_000_000_000, false);
            stopped.kind = before;
            stopped.stint = 1;
            let mut flying = LapMetadata::interval(2, 20_000_000_000, 100_000_000_000, true);
            flying.kind = LapKind::Flying;
            flying.stint = 1;
            flying.start_boundary = LapBoundary::CounterCrossing;
            flying.end_boundary = LapBoundary::CounterCrossing;
            let report = audit_track(
                &stored_with_duration(vec![stopped, flying], 100),
                &TrackAuditOptions::default(),
            )
            .unwrap();
            assert!(report
                .findings
                .iter()
                .any(|f| f.code == "impossible-transition"));
        }
    }

    fn stored_with_duration(laps: Vec<LapMetadata>, seconds: u64) -> Stored {
        Stored {
            laps,
            channels: vec![Channel {
                id: 0,
                name: "Clock".into(),
                unit: "s".into(),
                unit_source: UnitSource::Declared,
                sample_type: SampleType::F64,
                sample_count: seconds,
                duration_ns: seconds * 1_000_000_000,
                chunks: vec![Chunk {
                    time_base_ns: 0,
                    sample_base: 0,
                    data_ptr: 0,
                    sample_count: seconds,
                    sample_period_ns: 1_000_000_000,
                }],
            }],
        }
    }

    #[test]
    fn stale_merged_out_annotation_disagrees_with_native_pit_and_departure() {
        struct Native {
            stored: Stored,
            values: Vec<f64>,
        }
        impl TelemetrySource for Native {
            fn path(&self) -> &'static str {
                "stale-out"
            }
            fn format(&self) -> &'static str {
                "stored"
            }
            fn channels(&self) -> &[Channel] {
                &self.stored.channels
            }
            fn decode(&self, _: usize, _: usize, local: u64) -> f64 {
                self.values[local as usize]
            }
            fn source_lap_metadata(&self) -> Option<SourceLapMetadata> {
                self.stored.source_lap_metadata()
            }
        }
        let mut out = LapMetadata::interval(1, 0, 371_000_000_000, false);
        out.kind = LapKind::Out;
        out.stint = 1;
        let mut stored = stored_with_duration(vec![out], 400);
        stored.channels[0].name = "Lap_Number".into();
        stored.channels[0].unit.clear();
        let source = Native {
            stored,
            values: (0..400).map(|t| if t < 200 { 0.0 } else { 1.0 }).collect(),
        };
        let report = audit_track(&source, &TrackAuditOptions::default()).unwrap();
        assert!(
            report
                .findings
                .iter()
                .any(|f| f.code == "native-lap-disagreement"),
            "{report:?}"
        );
        assert!(report
            .findings
            .iter()
            .any(|f| f.code == "ignored-counter-rearm"));
    }
}
