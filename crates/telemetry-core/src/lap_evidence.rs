//! Independent evidence for dash events. Geometry is used locally to check
//! beacon crossings, never to manufacture a progress channel or smooth GPS.

use crate::{metadata::samples, names, TelemetrySource};

const SECOND: u64 = 1_000_000_000;

pub(crate) struct Evidence {
    speed: Vec<(u64, f64)>,
    cruise: f64,
    gate: Option<Gate>,
    pub(crate) pit_visits: Vec<(u64, u64)>,
}

struct Gate {
    origin: (f64, f64),
    direction: (f64, f64),
    fixes: Vec<(u64, f64, f64)>,
}

fn median(values: &mut [f64]) -> f64 {
    values.sort_by(f64::total_cmp);
    values[values.len() / 2]
}

fn xy(lat: f64, lon: f64, origin: (f64, f64)) -> (f64, f64) {
    (
        (lon - origin.1) * origin.0.to_radians().cos() * 111_195.0,
        (lat - origin.0) * 111_195.0,
    )
}

impl Evidence {
    pub(crate) fn new(source: &dyn TelemetrySource, counter: usize) -> Self {
        // Relative motion within the same native channel works even when CAN
        // units are undeclared. Exact zero remains the only parked evidence.
        let speed = names::find(
            source.channels(),
            &[
                "groundspeed",
                "speedref",
                "corrspeed",
                "vehiclespeed",
                "wheelspeed",
                "speedwspdapp",
                "vehrefspeed",
                "vcar",
                "speed",
            ],
        )
        .map_or_else(Vec::new, |i| samples(source, i));
        let mut positive: Vec<f64> = speed
            .iter()
            .map(|(_, v)| *v)
            .filter(|v| v.is_finite() && *v > 0.0)
            .collect();
        positive.sort_by(f64::total_cmp);
        let cruise = positive
            .get(positive.len() * 4 / 5)
            .copied()
            .unwrap_or(f64::INFINITY);
        let gate = Gate::new(source, counter);
        let pit_visits = pit_visits(source, counter);
        Self {
            speed,
            cruise,
            gate,
            pit_visits,
        }
    }

    pub(crate) fn initially_parked(&self) -> bool {
        let Some(&(start, value)) = self.speed.first() else {
            return false;
        };
        let end = self
            .speed
            .partition_point(|(t, _)| *t <= start.saturating_add(3 * SECOND));
        let values = &self.speed[..end];
        value == 0.0
            && values.iter().all(|(_, v)| *v == 0.0)
            && values
                .last()
                .is_some_and(|(t, _)| *t >= start.saturating_add(2 * SECOND))
            && values.windows(2).all(|w| w[1].0 - w[0].0 <= SECOND)
    }

    /// A sustained departure, corroborated by subsequent circuit-speed motion.
    /// A crawl or counter re-arm alone never establishes an out-lap. The
    /// boundary is backdated to the last observed stop, not the threshold.
    pub(crate) fn departure(&self, start: u64, end: u64) -> Option<u64> {
        let mut stopped = None;
        let mut movement = None;
        let mut fast = None;
        let mut previous = None;
        for &(t, v) in &self.speed {
            if t < start || t >= end {
                continue;
            }
            if !v.is_finite() || v < 0.0 || previous.is_some_and(|p| t - p > SECOND) {
                stopped = None;
                movement = None;
                fast = None;
            }
            previous = Some(t);
            if v == 0.0 {
                stopped = Some(t);
                movement = None;
                fast = None;
            } else if v > 0.0 {
                if movement.is_none() && stopped.is_some() {
                    movement = Some(t);
                }
                if v >= self.cruise * 0.5 {
                    let begin = *fast.get_or_insert(t);
                    if t - begin >= 3 * SECOND {
                        return movement;
                    }
                } else {
                    fast = None;
                }
            }
        }
        None
    }

    pub(crate) fn moving_before(&self, time: u64, window: u64) -> bool {
        let start = time.saturating_sub(window);
        let values: Vec<_> = self
            .speed
            .iter()
            .filter(|(t, _)| *t >= start && *t < time)
            .collect();
        values.first().is_some_and(|(t, _)| *t <= start + SECOND)
            && values.last().is_some_and(|(t, _)| *t + SECOND >= time)
            && values.windows(2).all(|w| w[1].0 - w[0].0 <= SECOND)
            && values.iter().all(|(_, v)| v.is_finite() && *v > 0.0)
            && values
                .iter()
                .filter(|(_, v)| *v >= self.cruise * 0.5)
                .count()
                * 4
                >= values.len() * 3
    }

    pub(crate) fn slow_before(&self, time: u64) -> bool {
        let values: Vec<_> = self
            .speed
            .iter()
            .filter(|(t, _)| *t >= time.saturating_sub(3 * SECOND) && *t < time)
            .collect();
        values
            .first()
            .is_some_and(|(t, _)| *t <= time.saturating_sub(2 * SECOND))
            && values.last().is_some_and(|(t, _)| *t + SECOND >= time)
            && values.windows(2).all(|w| w[1].0 - w[0].0 <= SECOND)
            && values
                .iter()
                .all(|(_, v)| v.is_finite() && *v >= 0.0 && *v < self.cruise * 0.4)
    }

    /// `None` means the GPS cannot adjudicate this event, not a rejection.
    pub(crate) fn crossing(&self, time: u64) -> Option<bool> {
        self.gate.as_ref()?.crossing(time)
    }
}

impl Gate {
    #[allow(
        clippy::float_cmp,
        reason = "Receiver fix and lap counters are exact integer codes"
    )]
    fn new(source: &dyn TelemetrySource, counter: usize) -> Option<Self> {
        let lat = names::find(source.channels(), &["gpslatitude", "latitude"])?;
        let velocity = names::find(source.channels(), &["gpsspeed"])?;
        let fixes: Vec<_> = samples(source, lat)
            .into_iter()
            .filter_map(|(t, _)| crate::track::trusted_position(source, t).map(|(a, b)| (t, a, b)))
            .collect();
        let &(_, latitude, longitude) = fixes.first()?;
        let matched = motorsport_track_atlas::match_track(latitude, longitude, 10_000.0)?;
        let geo: serde_json::Value =
            serde_json::from_str(matched.layout.centerline_geojson).ok()?;
        let features = geo.get("features")?.as_array()?;
        let sf = features
            .iter()
            .find(|f| f["properties"]["role"] == "start_finish")?["geometry"]["coordinates"]
            .as_array()?;
        let reference = (sf.get(1)?.as_f64()?, sf.first()?.as_f64()?);
        let line = features
            .iter()
            .find(|f| f["properties"]["role"] == "outline")?["geometry"]["coordinates"]
            .as_array()?;
        let next = line.get(1)?.as_array()?;
        let direction = xy(next.get(1)?.as_f64()?, next.first()?.as_f64()?, reference);
        let length = direction.0.hypot(direction.1);
        if length < 1.0 {
            return None;
        }
        // The dash beacon can precede the atlas start/finish. Calibrate its
        // local gate from several fast, accurate native crossings near S/F;
        // never assume their coordinates equal the atlas landmark.
        let counter_values = samples(source, counter);
        let mut lats = Vec::new();
        let mut lons = Vec::new();
        for pair in counter_values.windows(2) {
            let (t, v) = pair[1];
            if v < 2.0 || v != pair[0].1 + 1.0 {
                continue;
            }
            let Some(i) = fixes.partition_point(|(at, _, _)| *at <= t).checked_sub(1) else {
                continue;
            };
            let (at, a, b) = fixes[i];
            let (x, y) = xy(a, b, reference);
            if t - at <= SECOND / 5
                && x.hypot(y) <= 160.0
                && source
                    .sample_at(velocity, t, false)
                    .and_then(|v| crate::convert(v, &source.channels()[velocity].unit, "m/s").ok())
                    .is_some_and(|v| v.is_finite() && v > 35.0)
            {
                lats.push(a);
                lons.push(b);
            }
        }
        if lats.len() < 3 {
            return None;
        }
        let origin = (median(&mut lats), median(&mut lons));
        Some(Self {
            origin,
            direction: (direction.0 / length, direction.1 / length),
            fixes,
        })
    }

    fn crossing(&self, time: u64) -> Option<bool> {
        let start = self
            .fixes
            .partition_point(|(t, _, _)| *t < time.saturating_sub(3 * SECOND));
        let end = self
            .fixes
            .partition_point(|(t, _, _)| *t <= time.saturating_add(3 * SECOND));
        for pair in self.fixes[start..end].windows(2) {
            if pair[1].0 - pair[0].0 > SECOND / 4 {
                continue;
            }
            let (x0, y0) = xy(pair[0].1, pair[0].2, self.origin);
            let (x1, y1) = xy(pair[1].1, pair[1].2, self.origin);
            let along0 = x0 * self.direction.0 + y0 * self.direction.1;
            let along1 = x1 * self.direction.0 + y1 * self.direction.1;
            if along0 <= 0.0 && along1 > 0.0 && along1 - along0 < 30.0 {
                let u = -along0 / (along1 - along0);
                let lateral = ((x0 + u * (x1 - x0)) * self.direction.1
                    - (y0 + u * (y1 - y0)) * self.direction.0)
                    .abs();
                // A remote/drifting trace is missing evidence; a nearby
                // parallel pit-lane crossing contradicts a track beacon.
                if lateral <= 60.0 {
                    return Some(lateral <= 18.0);
                }
            }
        }
        None
    }
}

/// A moving pit visit needs spatial evidence independent of the dash: a
/// separate, slow lane through S/F, a displaced entry approach and a return
/// to the circuit beyond the atlas exit. Two fast native counter events
/// bound the observed track lane; reported GPS error enlarges that envelope.
/// No track lane geometry is inferred from a slow trace alone.
#[allow(clippy::float_cmp, reason = "Counter samples are exact integer codes")]
fn pit_visits(source: &dyn TelemetrySource, counter: usize) -> Vec<(u64, u64)> {
    use crate::track::{trusted_position, TrackGeometry, TrackPosition};
    struct Fix {
        time: u64,
        position: TrackPosition,
        speed: f64,
        error: f64,
    }
    let detect = || -> Option<Vec<(u64, u64)>> {
        let lat = names::find(source.channels(), &["gpslatitude", "latitude"])?;
        let velocity = names::find(source.channels(), &["gpsspeed"])?;
        let accuracy = names::find(source.channels(), &["gpspositionaccuracy"])?;
        let speed_at = |t| {
            source
                .sample_at(velocity, t, false)
                .and_then(|v| crate::convert(v, &source.channels()[velocity].unit, "m/s").ok())
        };
        let native = samples(source, lat);
        let (a, b) = native
            .iter()
            .find_map(|(t, _)| trusted_position(source, *t))?;
        let matched = motorsport_track_atlas::match_track(a, b, 10_000.0)?;
        let geometry = TrackGeometry::new(matched.layout)?;
        let (entry, exit) = geometry.pit_markers()?;
        // The detector needs a pit lane which straddles the atlas S/F.
        // Other layouts remain explicit evidence limits, not guessed visits.
        if entry <= exit || entry < 0.75 || exit > 0.25 {
            return None;
        }
        let mut references = Vec::new();
        for pair in samples(source, counter).windows(2) {
            let (time, value) = pair[1];
            if value != pair[0].1 + 1.0
                || value < 1.0
                || speed_at(time).is_none_or(|v| !v.is_finite() || v < 35.0)
            {
                continue;
            }
            let Some((a, b)) = trusted_position(source, time) else {
                continue;
            };
            let p = geometry.locate(a, b)?;
            if p.distance_m <= 12.0
                && (p.progress > 0.95 || p.progress < 0.05)
                && references
                    .last()
                    .is_none_or(|&(t, _)| time - t >= 20 * SECOND)
            {
                references.push((time, p.signed_distance_m));
            }
        }
        if references.len() < 2 {
            return None;
        }
        let low = references
            .iter()
            .map(|(_, v)| *v)
            .fold(f64::INFINITY, f64::min);
        let high = references
            .iter()
            .map(|(_, v)| *v)
            .fold(f64::NEG_INFINITY, f64::max);
        if high - low > 20.0 {
            return None;
        }
        let mut fixes = Vec::new();
        let mut previous = None;
        for (time, _) in native {
            if previous.is_some_and(|t| time - t < SECOND / 5) {
                continue;
            }
            previous = Some(time);
            let Some((a, b)) = trusted_position(source, time) else {
                continue;
            };
            let Some(speed) = speed_at(time) else {
                continue;
            };
            let Some(error) = source.sample_at(accuracy, time, false) else {
                continue;
            };
            fixes.push(Fix {
                time,
                position: geometry.locate(a, b)?,
                speed,
                error,
            });
        }
        let lane = |f: &Fix| {
            let margin = 10.0 + 2.0 * f.error;
            f.position.pit_sector
                && f.position.distance_m <= 60.0
                && (3.0..=25.0).contains(&f.speed)
                && (f.position.signed_distance_m < low - margin
                    || f.position.signed_distance_m > high + margin)
        };
        let mut visits = Vec::new();
        for i in 1..fixes.len() {
            let before = &fixes[i - 1];
            let now = &fixes[i];
            if before.position.progress < 0.98
                || now.position.progress > 0.02
                || now.time - before.time > SECOND / 4
                || !lane(before)
                || !lane(now)
            {
                continue;
            }
            let mut a = i - 1;
            let mut b = i;
            while a > 0 && lane(&fixes[a - 1]) && fixes[a].time - fixes[a - 1].time <= SECOND / 4 {
                a -= 1;
            }
            while b + 1 < fixes.len()
                && lane(&fixes[b + 1])
                && fixes[b + 1].time - fixes[b].time <= SECOND / 4
            {
                b += 1;
            }
            if fixes[b].time - fixes[a].time < 8 * SECOND {
                continue;
            }
            // A wide entry-side divergence corroborates the separate lane;
            // ordinary slow circuit motion near S/F cannot create pit state.
            if !fixes[a..i].windows(2).any(|pair| {
                pair[1].time - pair[0].time <= SECOND / 4
                    && pair.iter().all(|f| f.position.distance_m >= 25.0)
            }) {
                continue;
            }
            let Some(enter) = (1..i).rev().find(|&j| {
                let p = &fixes[j - 1];
                let q = &fixes[j];
                now.time - p.time <= 45 * SECOND
                    && q.time - p.time <= SECOND / 4
                    && p.position.progress < entry
                    && q.position.progress >= entry
                    && q.position.progress > 0.75
                    && q.position.distance_m >= 20.0
            }) else {
                continue;
            };
            let Some(leave) = (i + 1..fixes.len()).find(|&j| {
                let p = &fixes[j - 1];
                let q = &fixes[j];
                q.time - now.time <= 45 * SECOND
                    && q.time - p.time <= SECOND / 4
                    && p.position.progress <= exit
                    && q.position.progress > exit
                    && q.position.progress < 0.25
                    && q.position.distance_m <= 10.0
                    && q.speed > 25.0
            }) else {
                continue;
            };
            let start = fixes[enter].time;
            let end = fixes[leave].time;
            if !fixes[..enter]
                .iter()
                .rev()
                .take_while(|f| start - f.time <= 15 * SECOND)
                .any(|f| !f.position.pit_sector && f.position.distance_m <= 10.0 && f.speed > 10.0)
                || !references
                    .iter()
                    .any(|&(t, _)| t > end && t - end <= 180 * SECOND)
                || fixes[enter..=leave]
                    .windows(2)
                    .any(|p| p[1].time - p[0].time > SECOND / 4)
                || visits
                    .last()
                    .is_some_and(|&(_, previous_end)| start <= previous_end)
            {
                continue;
            }
            visits.push((start, end));
        }
        Some(visits)
    };
    detect().unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn acquisition_gaps_cannot_supply_stationary_or_departure_evidence() {
        let evidence = Evidence {
            speed: vec![
                (0, 0.0),
                (5 * SECOND, 0.0),
                (10 * SECOND, 200.0),
                (11 * SECOND, 200.0),
                (12 * SECOND, 200.0),
                (13 * SECOND, 200.0),
            ],
            cruise: 200.0,
            gate: None,
            pit_visits: Vec::new(),
        };
        assert!(!evidence.initially_parked());
        assert_eq!(evidence.departure(0, 14 * SECOND), None);
        assert!(!evidence.moving_before(13 * SECOND, 10 * SECOND));
    }
}
