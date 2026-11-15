//! What a step of the loop can fail with.
//!
//! A loop that has to read an error message to decide whether to retry has
//! been handed nothing, so a model failure carries `kind`, `transient`,
//! `status` and `request_id` as fields.

use crate::mapping::HarnessErrorInfo;

#[derive(Debug, Clone, thiserror::Error)]
pub enum Error {
    /// A model call failed, or a request was refused before it was sent.
    #[error("{message}")]
    Model {
        kind: &'static str,
        message: String,
        transient: bool,
        status: Option<u16>,
        request_id: Option<String>,
    },

    #[error("unknown stream handle {rid}")]
    UnknownStream { rid: u32 },

    #[error(
        "stream {rid} is already being polled elsewhere; a single call cannot be iterated \
         and read through its completion concurrently"
    )]
    StreamBusy { rid: u32 },

    #[error("no such tool `{name}`")]
    UnknownTool { name: String },

    #[error("tool `{name}` could not be dispatched: {message}")]
    ToolDispatchFailed { name: String, message: String },

    #[error("commit was not granted to this run")]
    CommitNotGranted,

    #[error("unknown function `{name}`")]
    UnknownFunction { name: String },

    #[error("function `{name}` failed: {message}")]
    FunctionFailed { name: String, message: String },
}

impl Error {
    /// The failure's kind, for the variants that classify one: a model
    /// failure or a refused request. `None` for everything else.
    pub fn kind(&self) -> Option<&'static str> {
        match self {
            Error::Model { kind, .. } => Some(kind),
            _ => None,
        }
    }

    /// Whether trying the same thing again could succeed.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            Error::Model {
                transient: true,
                ..
            }
        )
    }

    pub fn status(&self) -> Option<u16> {
        match self {
            Error::Model { status, .. } => *status,
            _ => None,
        }
    }

    pub fn request_id(&self) -> Option<&str> {
        match self {
            Error::Model { request_id, .. } => request_id.as_deref(),
            _ => None,
        }
    }
}

impl From<HarnessErrorInfo> for Error {
    fn from(info: HarnessErrorInfo) -> Self {
        Error::Model {
            kind: info.kind,
            message: info.message,
            transient: info.transient,
            status: info.status,
            request_id: info.request_id,
        }
    }
}

impl From<&llm::Error> for Error {
    fn from(error: &llm::Error) -> Self {
        HarnessErrorInfo::from(error).into()
    }
}

impl From<llm::Error> for Error {
    fn from(error: llm::Error) -> Self {
        Error::from(&error)
    }
}

impl From<&crate::validate::Refusal> for Error {
    fn from(refusal: &crate::validate::Refusal) -> Self {
        HarnessErrorInfo::from(refusal).into()
    }
}

impl From<crate::validate::Refusal> for Error {
    fn from(refusal: crate::validate::Refusal) -> Self {
        Error::from(&refusal)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_span_scope_error_carries_its_kind_as_a_field_not_the_message() {
        let error: Error = llm::Error::SpanScope {
            span_provider: "anthropic".into(),
            span_model: "claude".into(),
            request_provider: "openai".into(),
            request_model: "gpt".into(),
        }
        .into();
        assert_eq!(error.kind(), Some("span_scope"));
        assert!(!error.is_transient());
    }
}
