//! Diagnostics reported while loading, converting, or validating a recording.
//!
//! # Why this exists
//!
//! Vendor telemetry is routinely partially wrong: a firmware revision moves a
//! field, a logger is powered down mid-write, a Toolbox export drops a column.
//! Every format reader in this workspace therefore has to *recover* rather than
//! refuse, and recovery means choosing a value the file did not state.
//!
//! Recovering silently is the bug this module exists to prevent. A real example:
//! Pi/Cosworth definition records store the sample type code at a
//! layout-dependent offset. When the reader looked for it at the wrong offset it
//! found zero, fell back to "float64", and decoded every channel of a 1399
//! channel Daytona log as 8-byte doubles. `Speed_Wspd_App` came out as
//! 1.5e308 m/s. Nothing failed; the caller was simply handed nonsense, and the
//! file got blamed for being corrupt.
//!
//! A [`Diagnostic`] makes each such decision visible and attributable:
//! [`Severity::Error`] for data that could not be read, [`Severity::Warning`]
//! for a value that was assumed, clamped, or dropped, and [`Severity::Info`]
//! for a recovery worth noting but not acting on.
//!
//! # Codes
//!
//! Every diagnostic carries a stable machine-readable `code` so callers can
//! match on a condition without parsing prose. Codes are lowercase dotted
//! `<source>.<subject>_<problem>`, for example `pds.type_code_unreadable` or
//! `value.out_of_range`. `code` is `&'static str`: the set is fixed at compile
//! time, and emitting one must not allocate beyond its message.
//!
//! # Rules for emitters
//!
//! - Emit a warning whenever the returned data is not what the file stated:
//!   an assumed field, a clamped count, a skipped record, a substituted NaN.
//! - `message` names the concrete evidence (offset, channel, observed value),
//!   because a diagnostic that cannot be acted on is noise.
//! - Never emit a diagnostic *instead of* recovering, and never recover
//!   without emitting one.

use std::fmt;

/// How much a [`Diagnostic`] should worry the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Severity {
    /// A recovery worth recording that does not cast doubt on the data.
    Info,
    /// Data was assumed, clamped, substituted, or dropped. Values may be wrong.
    Warning,
    /// Data could not be read at all. The affected part is unusable.
    Error,
}

impl Severity {
    /// Returns a stable lowercase label.
    pub fn name(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

/// One reportable observation about a recording.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    /// How much this should worry the caller.
    pub severity: Severity,
    /// Stable lowercase dotted identifier, for example `pds.type_code_unreadable`.
    pub code: &'static str,
    /// Specific, evidence-bearing explanation for a human.
    pub message: String,
    /// Channel this concerns, when it concerns exactly one.
    pub channel: Option<String>,
}

impl Diagnostic {
    /// Builds a diagnostic with no channel attribution.
    pub fn new(severity: Severity, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            severity,
            code,
            message: message.into(),
            channel: None,
        }
    }

    /// Builds an [`Severity::Info`] diagnostic.
    pub fn info(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(Severity::Info, code, message)
    }

    /// Builds a [`Severity::Warning`] diagnostic.
    pub fn warning(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(Severity::Warning, code, message)
    }

    /// Builds a [`Severity::Error`] diagnostic.
    pub fn error(code: &'static str, message: impl Into<String>) -> Self {
        Self::new(Severity::Error, code, message)
    }

    /// Attributes this diagnostic to one channel.
    #[must_use]
    pub fn with_channel(mut self, channel: impl Into<String>) -> Self {
        self.channel = Some(channel.into());
        self
    }
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} [{}] ", self.severity.name(), self.code)?;
        if let Some(channel) = &self.channel {
            write!(f, "{channel}: ")?;
        }
        f.write_str(&self.message)
    }
}

/// An ordered set of diagnostics collected during one load or validation.
///
/// Emission order is preserved: it is the order the reader encountered the
/// problems, which is the most useful order for diagnosing a file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Diagnostics {
    items: Vec<Diagnostic>,
    /// Diagnostics dropped because [`Self::CAP`] was reached.
    suppressed: usize,
}

impl Diagnostics {
    /// Maximum retained diagnostics.
    ///
    /// A hostile or badly damaged file can make every one of a million records
    /// complain. Retaining a bounded prefix keeps a corrupt file from turning
    /// into an out-of-memory condition, which is itself a resilience bug.
    pub const CAP: usize = 1024;

    /// Creates an empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one diagnostic, or counts it as suppressed once at [`Self::CAP`].
    pub fn push(&mut self, diagnostic: Diagnostic) {
        if self.items.len() >= Self::CAP {
            self.suppressed += 1;
            return;
        }
        self.items.push(diagnostic);
    }

    /// Appends an [`Severity::Info`] diagnostic.
    pub fn info(&mut self, code: &'static str, message: impl Into<String>) {
        self.push(Diagnostic::info(code, message));
    }

    /// Appends a [`Severity::Warning`] diagnostic.
    pub fn warning(&mut self, code: &'static str, message: impl Into<String>) {
        self.push(Diagnostic::warning(code, message));
    }

    /// Appends an [`Severity::Error`] diagnostic.
    pub fn error(&mut self, code: &'static str, message: impl Into<String>) {
        self.push(Diagnostic::error(code, message));
    }

    /// Appends diagnostics from an iterator, preserving their order.
    ///
    /// Use [`Self::append`] when merging another [`Diagnostics`] so its
    /// already-suppressed count is preserved too.
    pub fn extend(&mut self, other: impl IntoIterator<Item = Diagnostic>) {
        for diagnostic in other {
            self.push(diagnostic);
        }
    }

    /// Merges another set, preserving order and both suppressed counts.
    pub fn append(&mut self, other: Self) {
        let Self { items, suppressed } = other;
        self.extend(items);
        self.suppressed = self.suppressed.saturating_add(suppressed);
    }

    /// Returns the retained diagnostics in emission order.
    pub fn items(&self) -> &[Diagnostic] {
        &self.items
    }

    /// Returns how many diagnostics were dropped at [`Self::CAP`].
    pub fn suppressed(&self) -> usize {
        self.suppressed
    }

    /// Returns whether nothing was reported.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Returns the number of retained diagnostics.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Counts retained diagnostics of one severity.
    pub fn count(&self, severity: Severity) -> usize {
        self.items
            .iter()
            .filter(|item| item.severity == severity)
            .count()
    }

    /// Returns the highest severity reported, if any.
    pub fn max_severity(&self) -> Option<Severity> {
        self.items.iter().map(|item| item.severity).max()
    }

    /// Returns whether any [`Severity::Error`] was reported.
    pub fn has_errors(&self) -> bool {
        self.items
            .iter()
            .any(|item| item.severity == Severity::Error)
    }

    /// Returns the first diagnostic carrying `code`.
    pub fn find(&self, code: &str) -> Option<&Diagnostic> {
        self.items.iter().find(|item| item.code == code)
    }

    /// Consumes the set and returns the retained diagnostics.
    pub fn into_items(self) -> Vec<Diagnostic> {
        self.items
    }
}

impl IntoIterator for Diagnostics {
    type Item = Diagnostic;
    type IntoIter = std::vec::IntoIter<Diagnostic>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.into_iter()
    }
}

impl<'a> IntoIterator for &'a Diagnostics {
    type Item = &'a Diagnostic;
    type IntoIter = std::slice::Iter<'a, Diagnostic>;
    fn into_iter(self) -> Self::IntoIter {
        self.items.iter()
    }
}

impl FromIterator<Diagnostic> for Diagnostics {
    fn from_iter<I: IntoIterator<Item = Diagnostic>>(iter: I) -> Self {
        let mut out = Self::new();
        out.extend(iter);
        out
    }
}

impl fmt::Display for Diagnostics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, item) in self.items.iter().enumerate() {
            if index > 0 {
                f.write_str("\n")?;
            }
            write!(f, "{item}")?;
        }
        if self.suppressed > 0 {
            if !self.items.is_empty() {
                f.write_str("\n")?;
            }
            write!(f, "... {} further diagnostics suppressed", self.suppressed)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn severity_orders_info_below_error() {
        assert!(Severity::Info < Severity::Warning);
        assert!(Severity::Warning < Severity::Error);
    }

    #[test]
    fn max_severity_reports_worst_not_last() {
        let mut diagnostics = Diagnostics::new();
        diagnostics.error("t.error", "unreadable");
        diagnostics.info("t.info", "recovered");
        assert_eq!(diagnostics.max_severity(), Some(Severity::Error));
        assert!(diagnostics.has_errors());
        assert_eq!(diagnostics.count(Severity::Info), 1);
    }

    #[test]
    fn cap_bounds_retained_items_and_counts_the_rest() {
        let mut diagnostics = Diagnostics::new();
        for _ in 0..Diagnostics::CAP + 25 {
            diagnostics.warning("t.flood", "damaged record");
        }
        assert_eq!(diagnostics.len(), Diagnostics::CAP);
        assert_eq!(diagnostics.suppressed(), 25);
        assert!(diagnostics
            .to_string()
            .ends_with("25 further diagnostics suppressed"));
    }

    #[test]
    fn append_preserves_existing_and_new_suppressed_counts() {
        let mut left = Diagnostics::new();
        for _ in 0..Diagnostics::CAP + 2 {
            left.warning("left.flood", "left");
        }
        let mut right = Diagnostics::new();
        for _ in 0..Diagnostics::CAP + 3 {
            right.warning("right.flood", "right");
        }
        left.append(right);
        assert_eq!(left.len(), Diagnostics::CAP);
        assert_eq!(left.suppressed(), Diagnostics::CAP + 5);
    }

    #[test]
    fn display_names_channel_and_code() {
        let diagnostic = Diagnostic::warning("pds.type_code_unreadable", "assumed float32")
            .with_channel("Speed_Wspd_App");
        assert_eq!(
            diagnostic.to_string(),
            "warning [pds.type_code_unreadable] Speed_Wspd_App: assumed float32"
        );
    }

    #[test]
    fn find_locates_by_code() {
        let mut diagnostics = Diagnostics::new();
        diagnostics.warning("a.one", "first");
        diagnostics.warning("b.two", "second");
        assert_eq!(
            diagnostics.find("b.two").map(|d| d.message.as_str()),
            Some("second")
        );
        assert!(diagnostics.find("c.three").is_none());
    }
}
