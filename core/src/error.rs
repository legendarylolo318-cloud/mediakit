use std::path::PathBuf;

/// Errors surfaced by the core engine. Where an ffmpeg/ffprobe subprocess is
/// involved, the real stderr output is always carried along so the GUI/CLI
/// can show the user what actually went wrong instead of a bare failure.
#[derive(Debug, thiserror::Error)]
pub enum CoreError {
    #[error(
        "ffmpeg/ffprobe binary not found (checked app data dir, next to executable, and PATH)"
    )]
    FfmpegNotFound,

    #[error("failed to launch `{binary}`: {source}")]
    Spawn {
        binary: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("ffprobe failed on `{path}`:\n{stderr}")]
    ProbeFailed { path: PathBuf, stderr: String },

    #[error("failed to parse ffprobe output for `{path}`: {source}")]
    ProbeParse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("ffmpeg encode failed (exit code {code:?}):\n{stderr}")]
    EncodeFailed { code: Option<i32>, stderr: String },

    #[error("job was cancelled")]
    Cancelled,

    #[error("could not determine a per-OS app data directory")]
    NoAppDataDir,

    #[error("io error at `{path}`: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },

    #[error("download of ffmpeg failed: {0}")]
    Download(String),

    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

pub type CoreResult<T> = Result<T, CoreError>;
