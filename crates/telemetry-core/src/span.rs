//! Interval annotations on the file-relative nanosecond axis.

use crate::timespan::{format_timespan_ms, parse_timespan_ms, TIMESPAN_MS, TIMESPAN_MS_MAX};

/// One interval: `[start_ns, end_ns)` plus display metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    /// Optional stable id (`443-stint-1`).
    pub name: String,
    /// Inclusive start, file-relative nanoseconds.
    pub start_ns: u64,
    /// Exclusive end, file-relative nanoseconds.
    pub end_ns: u64,
    /// Default visibility.
    pub visible: bool,
    /// `#RRGGBB`, empty if unset.
    pub color: String,
    /// On-span chrome (`primary.title` / `primary.subtitle`).
    pub primary: SpanPrimary,
    /// On-hover fields, in file order.
    pub meta: Vec<(String, SpanMetaValue)>,
}

/// Labels drawn on the span itself.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SpanPrimary {
    /// Main label, e.g. `#443`.
    pub title: String,
    /// Secondary label, e.g. `EL · 1:52.1`.
    pub subtitle: String,
}

/// One hover-field value. Race times are integer milliseconds so they can be
/// averaged; everything else stays a string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SpanMetaValue {
    /// Free text (`IMSA`, `28`, a driver name).
    Text(String),
    /// Race duration. Integer milliseconds, `0..=`[`TIMESPAN_MS_MAX`].
    /// Renders as [`format_timespan_ms`].
    TimeMs(u32),
}

impl SpanMetaValue {
    /// Interpret a JSONL / catalog string: racing times become [`Self::TimeMs`].
    pub fn from_stored_text(text: String) -> Self {
        parse_timespan_ms(&text)
            .map(Self::TimeMs)
            .unwrap_or(Self::Text(text))
    }

    /// Integer milliseconds when this value is a race time.
    pub fn as_timespan_ms(&self) -> Option<u32> {
        match self {
            Self::TimeMs(ms) => Some(*ms),
            Self::Text(text) => parse_timespan_ms(text),
        }
    }

    /// Viewer string: `M:SS.FFF` / `H:MM:SS.FFF` for times, otherwise the text.
    pub fn display(&self) -> String {
        match self {
            Self::Text(text) => text.clone(),
            Self::TimeMs(ms) => format_timespan_ms(*ms),
        }
    }

    /// Canonical unit of a typed value (`timespan_ms`), or empty for text.
    pub fn unit(&self) -> &'static str {
        match self {
            Self::Text(_) => "",
            Self::TimeMs(_) => TIMESPAN_MS,
        }
    }
}

impl From<&str> for SpanMetaValue {
    fn from(text: &str) -> Self {
        Self::Text(text.to_owned())
    }
}

impl From<String> for SpanMetaValue {
    fn from(text: String) -> Self {
        Self::Text(text)
    }
}

impl From<u32> for SpanMetaValue {
    fn from(ms: u32) -> Self {
        Self::TimeMs(ms.min(TIMESPAN_MS_MAX))
    }
}
