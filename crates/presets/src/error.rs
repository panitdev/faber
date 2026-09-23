use thiserror::Error;

/// Why a catalog could not be read.
///
/// A value, not a policy: whether a service runs without presets or refuses
/// to start is the caller's decision, and the two cases here — "the network
/// said no" and "the bytes weren't the directory" — are worth telling apart.
#[derive(Debug, Error)]
pub enum Error {
    /// The directory could not be reached. `reqwest` carries the detail:
    /// a refused connection, a timeout, a non-2xx status.
    #[error("could not fetch the model directory: {0}")]
    Http(#[from] reqwest::Error),
    /// The directory was reached but its body was not the expected JSON.
    #[error("the model directory is not valid JSON: {0}")]
    Decode(#[from] serde_json::Error),
}
