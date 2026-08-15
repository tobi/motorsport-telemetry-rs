//! Interval annotations on the file-relative nanosecond axis.

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
    /// On-hover fields, in file order. Each pair is (name, value) strings.
    pub meta: Vec<(String, String)>,
}

/// Labels drawn on the span itself.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SpanPrimary {
    /// Main label, e.g. `#443`.
    pub title: String,
    /// Secondary label, e.g. `EL · 1:52.1`.
    pub subtitle: String,
}
