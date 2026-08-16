//! Provenance records for named, lossless processing passes.
//!
//! A pass reads channels that already exist and appends derived channels
//! under new names; it never mutates or removes source samples. Which passes
//! ran — with their parameters, inputs, and outputs — is persisted with the
//! recording so a viewer can show how a file was processed given the nature
//! of its source, and so the raw conversion can be rebuilt exactly by
//! dropping every channel named in [`AppliedPass::outputs`].
//!
//! Records are deterministic: same source, same pass version, same
//! parameters produce identical outputs and an identical record. They carry
//! no timestamps.

/// Provenance record for one processing pass applied to a recording.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppliedPass {
    /// Stable registry name, e.g. `gps.clean`.
    pub name: String,
    /// Implementation version that produced the outputs.
    ///
    /// Bumped whenever the pass changes its outputs for identical inputs.
    pub version: u32,
    /// Canonical `key=value` parameters the pass ran with, sorted by key.
    pub params: Vec<(String, String)>,
    /// Names of the channels the pass read.
    pub inputs: Vec<String>,
    /// Names of the channels the pass appended.
    pub outputs: Vec<String>,
}

impl AppliedPass {
    /// `name@version` display label, e.g. `gps.clean@1`.
    pub fn label(&self) -> String {
        format!("{}@{}", self.name, self.version)
    }
}

/// Identity of the original vendor recording a converted artifact came from.
///
/// Carried unchanged through every rewrite of a converted file so the chain
/// `vendor file -> .telemetry -> rewritten .telemetry` never forgets where
/// the samples originally came from.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceOrigin {
    /// Stable lowercase format identifier of the original, e.g. `aimd`.
    pub format: String,
    /// Path of the original recording as seen at first conversion.
    pub path: String,
}
