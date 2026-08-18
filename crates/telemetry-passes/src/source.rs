//! Attaching a pass's output to the accumulating [`ViewSource`].

use crate::{DerivedChannel, PassError, PassOutput};
use motorsport_telemetry_core::{AppliedPass, ViewError, ViewSource};

/// Appends a pass's output channels to `view` and records its provenance.
/// Returns the appended channel names.
pub(crate) fn push_pass(
    view: &mut ViewSource<'_>,
    name: &str,
    version: u32,
    output: PassOutput,
) -> Result<Vec<String>, PassError> {
    let pass_label = format!("{name}@{version}");
    let mut outputs = Vec::with_capacity(output.channels.len());
    for derived in output.channels {
        let DerivedChannel {
            name: channel_name,
            unit,
            sample_type,
            mirrors,
            data,
        } = derived;
        view.append(&channel_name, &unit, sample_type, mirrors, data)
            .map_err(|err| map_view_error(err, &pass_label))?;
        outputs.push(channel_name);
    }
    let mut params = output.params;
    params.sort_by(|a, b| a.0.cmp(&b.0));
    view.passes_mut().push(AppliedPass {
        name: name.to_owned(),
        version,
        params,
        inputs: output.inputs,
        outputs: outputs.clone(),
    });
    Ok(outputs)
}

/// Maps a [`ViewError`] into the matching [`PassError`], tagged with the
/// pass label so the caller knows which pass produced the bad output.
fn map_view_error(err: ViewError, pass_label: &str) -> PassError {
    match err {
        ViewError::BadMirror {
            mirrors,
            channel_count,
        } => PassError::BadMirror {
            pass: pass_label.to_owned(),
            mirrors,
            channel_count,
        },
        ViewError::OutputShape {
            channel,
            expected,
            actual,
        } => PassError::OutputShape {
            pass: pass_label.to_owned(),
            channel,
            expected,
            actual,
        },
        ViewError::DuplicateName(name) => PassError::DuplicateName {
            pass: pass_label.to_owned(),
            name,
        },
    }
}
