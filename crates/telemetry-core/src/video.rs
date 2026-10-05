//! Immutable, source-supported mappings between telemetry and linked videos.

use std::collections::BTreeMap;
use std::fmt;

/// One native synchronization observation, on two integer nanosecond axes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoSyncPoint {
    /// File-relative telemetry time.
    pub telemetry_time_ns: u64,
    /// Presentation time inside the linked video file.
    pub presentation_time_ns: u64,
}

/// A contiguous run of observations for one video, without a clock reset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoSyncSegment {
    /// Source-declared video index, matching [`crate::VideoFileRef::index`].
    pub file_index: u32,
    /// Strictly increasing telemetry times and nondecreasing presentation times.
    /// A single observation supports only that exact telemetry instant.
    pub points: Vec<VideoSyncPoint>,
}

/// The linked file and presentation timestamp at a telemetry instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VideoPosition {
    /// Source-declared video file index.
    pub file_index: u32,
    /// Presentation timestamp inside that file, never a nominal frame index.
    pub presentation_time_ns: u64,
}

/// Why a normalized video mapping could not be established or queried.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoMappingError {
    /// The requested instant lies outside the supported observations.
    Unmapped,
    /// More than one telemetry instant corresponds to the requested video time.
    Ambiguous,
    /// The supplied segments have invalid indices, clocks, or overlapping bounds.
    InvalidTimeline,
}

impl fmt::Display for VideoMappingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Unmapped => "no recorded video mapping at this instant",
            Self::Ambiguous => "video time corresponds to multiple telemetry instants",
            Self::InvalidTimeline => "invalid video synchronization timeline",
        })
    }
}

impl std::error::Error for VideoMappingError {}

/// Checked, immutable native video synchronization, independent of decoded arrays.
///
/// Interpolation is restricted to observations in the same segment. There is no
/// extrapolation across a missing observation, file roll, gap, or clock reset.
/// Repeated presentation times are preserved; their inverse is ambiguous.
/// These bounds describe recorded clock support, **not** playable media extent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VideoTimeline {
    segments: Vec<VideoSyncSegment>,
    by_file: BTreeMap<u32, Vec<usize>>,
}

impl VideoTimeline {
    /// Validates root-order segments without changing or sorting observations.
    /// Each segment must be nonempty, use a positive file index, and follow the
    /// previous segment strictly in telemetry time. Presentation resets belong
    /// in separate segments, including resets inside the same video file.
    pub fn from_segments(segments: Vec<VideoSyncSegment>) -> Result<Self, VideoMappingError> {
        if segments.is_empty() {
            return Err(VideoMappingError::InvalidTimeline);
        }
        let mut last_time = None;
        let mut by_file = BTreeMap::<u32, Vec<usize>>::new();
        for (index, segment) in segments.iter().enumerate() {
            let Some(first) = segment.points.first() else {
                return Err(VideoMappingError::InvalidTimeline);
            };
            if segment.file_index == 0
                || last_time.is_some_and(|last| first.telemetry_time_ns <= last)
                || segment.points.windows(2).any(|pair| {
                    pair[0].telemetry_time_ns >= pair[1].telemetry_time_ns
                        || pair[0].presentation_time_ns > pair[1].presentation_time_ns
                })
            {
                return Err(VideoMappingError::InvalidTimeline);
            }
            last_time = segment.points.last().map(|point| point.telemetry_time_ns);
            by_file.entry(segment.file_index).or_default().push(index);
        }
        Ok(Self { segments, by_file })
    }

    /// Returns exact native segments in increasing telemetry order.
    pub fn segments(&self) -> &[VideoSyncSegment] {
        &self.segments
    }

    /// Maps a telemetry instant, without extrapolation outside native support.
    pub fn presentation_at(&self, time_ns: u64) -> Result<VideoPosition, VideoMappingError> {
        let after = self
            .segments
            .partition_point(|segment| segment.points[0].telemetry_time_ns <= time_ns);
        let segment = after
            .checked_sub(1)
            .and_then(|index| self.segments.get(index))
            .ok_or(VideoMappingError::Unmapped)?;
        let presentation_time_ns = interpolate(
            &segment.points,
            time_ns,
            |point| point.telemetry_time_ns,
            |point| point.presentation_time_ns,
        )?;
        Ok(VideoPosition {
            file_index: segment.file_index,
            presentation_time_ns,
        })
    }

    /// Maps a file-specific presentation time to telemetry. Repeated PTS values
    /// and overlapping presentation ranges after a reset return `Ambiguous`.
    pub fn telemetry_at(&self, file_index: u32, time_ns: u64) -> Result<u64, VideoMappingError> {
        let indices = self
            .by_file
            .get(&file_index)
            .ok_or(VideoMappingError::Unmapped)?;
        let mut answer = None;
        for &index in indices {
            let points = &self.segments[index].points;
            let start = points.partition_point(|point| point.presentation_time_ns < time_ns);
            let end = points.partition_point(|point| point.presentation_time_ns <= time_ns);
            if end.saturating_sub(start) > 1 {
                return Err(VideoMappingError::Ambiguous);
            }
            match interpolate(
                points,
                time_ns,
                |point| point.presentation_time_ns,
                |point| point.telemetry_time_ns,
            ) {
                Ok(time) if answer.is_none() => answer = Some(time),
                Ok(_) | Err(VideoMappingError::Ambiguous) => {
                    return Err(VideoMappingError::Ambiguous);
                }
                Err(VideoMappingError::Unmapped) => {}
                Err(error) => return Err(error),
            }
        }
        answer.ok_or(VideoMappingError::Unmapped)
    }
}

fn interpolate(
    points: &[VideoSyncPoint],
    time: u64,
    input: impl Fn(&VideoSyncPoint) -> u64,
    output: impl Fn(&VideoSyncPoint) -> u64,
) -> Result<u64, VideoMappingError> {
    let after = points.partition_point(|point| input(point) < time);
    if let Some(point) = points.get(after).filter(|point| input(point) == time) {
        return Ok(output(point));
    }
    let previous = after
        .checked_sub(1)
        .and_then(|index| points.get(index))
        .ok_or(VideoMappingError::Unmapped)?;
    let next = points.get(after).ok_or(VideoMappingError::Unmapped)?;
    let width = input(next) - input(previous);
    if width == 0 {
        return Err(VideoMappingError::Ambiguous);
    }
    let numerator =
        u128::from(time - input(previous)) * u128::from(output(next) - output(previous));
    let delta = (numerator + u128::from(width / 2)) / u128::from(width);
    output(previous)
        .checked_add(u64::try_from(delta).map_err(|_| VideoMappingError::InvalidTimeline)?)
        .ok_or(VideoMappingError::InvalidTimeline)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn segment(index: u32, points: &[(u64, u64)]) -> VideoSyncSegment {
        VideoSyncSegment {
            file_index: index,
            points: points
                .iter()
                .map(
                    |&(telemetry_time_ns, presentation_time_ns)| VideoSyncPoint {
                        telemetry_time_ns,
                        presentation_time_ns,
                    },
                )
                .collect(),
        }
    }

    #[test]
    fn preserves_nonzero_offsets_and_file_rolls_without_extrapolation() {
        let clock = VideoTimeline::from_segments(vec![
            segment(1, &[(0, 16_233_000_000), (40_000_000, 16_266_000_000)]),
            segment(2, &[(80_000_000, 0), (120_000_000, 33_000_000)]),
        ])
        .unwrap();
        assert_eq!(
            clock.presentation_at(20_000_000).unwrap(),
            VideoPosition {
                file_index: 1,
                presentation_time_ns: 16_249_500_000,
            }
        );
        assert_eq!(clock.telemetry_at(1, 16_249_500_000).unwrap(), 20_000_000);
        assert_eq!(
            clock.presentation_at(60_000_000),
            Err(VideoMappingError::Unmapped)
        );
        assert_eq!(clock.presentation_at(80_000_000).unwrap().file_index, 2);
        assert_eq!(clock.telemetry_at(2, 0).unwrap(), 80_000_000);
        assert_eq!(
            clock.telemetry_at(2, 34_000_000),
            Err(VideoMappingError::Unmapped)
        );
        assert_eq!(clock.telemetry_at(3, 0), Err(VideoMappingError::Unmapped));
    }

    #[test]
    fn repeated_pts_and_reset_ranges_have_no_unique_inverse() {
        let clock = VideoTimeline::from_segments(vec![
            segment(1, &[(0, 10), (20, 30), (40, 30), (60, 50)]),
            segment(1, &[(80, 10), (100, 20)]),
        ])
        .unwrap();
        assert_eq!(clock.presentation_at(30).unwrap().presentation_time_ns, 30);
        assert_eq!(clock.telemetry_at(1, 30), Err(VideoMappingError::Ambiguous));
        assert_eq!(clock.telemetry_at(1, 15), Err(VideoMappingError::Ambiguous));
        assert_eq!(clock.telemetry_at(1, 40).unwrap(), 50);
    }

    #[test]
    fn rejects_invalid_segments_and_supports_single_observations() {
        for segments in [
            vec![],
            vec![segment(0, &[(0, 0)])],
            vec![segment(1, &[])],
            vec![segment(1, &[(0, 1), (0, 2)])],
            vec![segment(1, &[(0, 2), (1, 1)])],
            vec![segment(1, &[(0, 0), (10, 10)]), segment(2, &[(10, 0)])],
        ] {
            assert_eq!(
                VideoTimeline::from_segments(segments),
                Err(VideoMappingError::InvalidTimeline)
            );
        }
        let clock = VideoTimeline::from_segments(vec![segment(1, &[(42, 88)])]).unwrap();
        assert_eq!(clock.telemetry_at(1, 88), Ok(42));
        assert_eq!(clock.presentation_at(41), Err(VideoMappingError::Unmapped));
    }

    #[test]
    fn interpolation_remains_exact_near_u64_limits() {
        let clock = VideoTimeline::from_segments(vec![segment(1, &[(0, 0), (u64::MAX, u64::MAX)])])
            .unwrap();
        assert_eq!(
            clock
                .presentation_at(u64::MAX - 1)
                .unwrap()
                .presentation_time_ns,
            u64::MAX - 1
        );
        assert_eq!(clock.telemetry_at(1, u64::MAX - 1), Ok(u64::MAX - 1));
    }
}
