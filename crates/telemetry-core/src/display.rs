//! How a channel should be drawn. Independent of sample storage.

/// Plot class. Omitted / [`Self::Trace`] is a normal Y-vs-time strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChannelPlot {
    /// Speed, throttle, brake, steering, and other overlay traces.
    /// Comment labels (`lbl`) are allowed only here.
    #[default]
    Trace,
    /// Temperature, BPM, SpO2, and other scalar foreign signals.
    Gauge,
    /// Circular quantities: wind direction, heading. Wraps at 360°.
    Compass,
}

impl ChannelPlot {
    /// Wire name used in JSONL (`plt`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Trace => "trace",
            Self::Gauge => "gauge",
            Self::Compass => "compass",
        }
    }

    /// Parse a `plt` token. Empty / unknown is [`None`], not a guess.
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "trace" | "t" => Some(Self::Trace),
            "gauge" | "g" => Some(Self::Gauge),
            "compass" | "c" => Some(Self::Compass),
            _ => None,
        }
    }

    /// True when this class is a normal overlay trace.
    pub fn is_trace(self) -> bool {
        matches!(self, Self::Trace)
    }
}

/// Optional display scale, rounding, and plot class for one channel.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ChannelDisplay {
    /// How to draw the channel. Default is a time-series trace.
    pub plot: ChannelPlot,
    /// Suggested axis / gauge minimum, when the writer set one.
    pub scale_min: Option<f64>,
    /// Suggested axis / gauge maximum, when the writer set one.
    pub scale_max: Option<f64>,
    /// Decimal places to show. `None` means the viewer picks.
    pub decimals: Option<u8>,
    /// Format hint such as `0.0°C` or `000`. No whitespace. Empty if unset.
    pub format: String,
}

impl ChannelDisplay {
    /// Default trace with no scale or rounding.
    pub fn trace() -> Self {
        Self::default()
    }

    /// True when every field is the default (omit from JSONL).
    pub fn is_default(&self) -> bool {
        self.plot.is_trace()
            && self.scale_min.is_none()
            && self.scale_max.is_none()
            && self.decimals.is_none()
            && self.format.is_empty()
    }
}
