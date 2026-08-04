//! Types describing a queued/running conversion job. The engine (see
//! [`crate::engine`]) is the only thing that mutates job state; the GUI just
//! renders whatever the latest [`EngineEvent`](crate::engine::EngineEvent)
//! told it.

use crate::command::{EncodePass, EncodeSettings};
use std::time::Duration;

pub type JobId = u64;

#[derive(Debug, Clone, PartialEq)]
pub enum JobState {
    Queued,
    Running,
    Done,
    Failed(String),
    Cancelled,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct JobProgressInfo {
    pub percent: Option<f64>,
    pub eta: Option<Duration>,
    pub speed: Option<f64>,
    pub pass_index: usize,
    pub pass_count: usize,
}

/// When set on a [`JobSpec`], the engine checks the actual output file size
/// after a successful encode and, if it's still over `target_bytes`, retries
/// with a proportionally reduced video bitrate (up to `max_retries` times).
#[derive(Debug, Clone)]
pub struct TargetSizePolicy {
    pub target_bytes: u64,
    pub safety_margin: f64,
    pub max_retries: u8,
}

/// Everything the engine needs to run one job, independent of any GUI state.
#[derive(Debug, Clone)]
pub struct JobSpec {
    pub id: JobId,
    pub settings: EncodeSettings,
    /// `[Single]` for a normal encode, or `[First { .. }, Second { .. }]`
    /// for a two-pass bitrate-targeted encode.
    pub passes: Vec<EncodePass>,
    /// Source duration in seconds, used to turn `out_time` into a percentage
    /// and ETA. `0.0` if unknown (progress will just show raw stats).
    pub total_duration_seconds: f64,
    /// Present for target-size presets (Discord 10/50/500 MB, custom target
    /// size); `None` for ordinary encodes.
    pub target_size: Option<TargetSizePolicy>,
}
