use std::{
    io::{self, Read, Write},
    time::Duration,
};

use aizu_core::{
    ApprovalDecision, LocalApprovalRequest, LocalApprovalResponse, MAX_LOCAL_APPROVAL_FRAME_BYTES,
    StatePaths, parse_strict_json_value,
};
use thiserror::Error;

// Leave time for the broker's 45-second decision deadline to return an
// explicit `presented` fallback before the agent's 50-second hook deadline.
const APPROVAL_WAIT_TIMEOUT: Duration = Duration::from_secs(48);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ApprovalClientOutcome {
    pub presented: bool,
    pub decision: Option<ApprovalDecision>,
    pub answer: Option<u16>,
}

#[derive(Debug, Error)]
pub enum ApprovalClientError {
    #[error("the local approval broker is unavailable: {0}")]
    Io(#[from] io::Error),
    #[error("the local approval frame is too large")]
    FrameTooLarge,
    #[error("the local approval response is invalid: {0}")]
    InvalidResponse(String),
}

#[cfg(unix)]
pub fn request(
    paths: &StatePaths,
    request: &LocalApprovalRequest,
) -> Result<ApprovalClientOutcome, ApprovalClientError> {
    use std::os::unix::net::UnixStream;

    let mut stream = UnixStream::connect(paths.approval_socket())?;
    stream.set_read_timeout(Some(APPROVAL_WAIT_TIMEOUT))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    let encoded = serde_json::to_vec(request)
        .map_err(|error| ApprovalClientError::InvalidResponse(error.to_string()))?;
    if encoded.len() > MAX_LOCAL_APPROVAL_FRAME_BYTES {
        return Err(ApprovalClientError::FrameTooLarge);
    }
    stream.write_all(&encoded)?;
    stream.write_all(b"\n")?;
    stream.flush()?;

    let mut response = Vec::new();
    stream
        .take((MAX_LOCAL_APPROVAL_FRAME_BYTES + 2) as u64)
        .read_to_end(&mut response)?;
    if response.len() > MAX_LOCAL_APPROVAL_FRAME_BYTES + 1 {
        return Err(ApprovalClientError::FrameTooLarge);
    }
    if response.last() == Some(&b'\n') {
        response.pop();
    }
    if response.is_empty() || response.contains(&b'\n') {
        return Err(ApprovalClientError::InvalidResponse(
            "expected exactly one response frame".to_owned(),
        ));
    }
    let value = parse_strict_json_value(&response, MAX_LOCAL_APPROVAL_FRAME_BYTES)
        .map_err(|error| ApprovalClientError::InvalidResponse(error.to_string()))?;
    let response: LocalApprovalResponse = serde_json::from_value(value)
        .map_err(|error| ApprovalClientError::InvalidResponse(error.to_string()))?;
    if response.request_id() != request.request_id {
        return Err(ApprovalClientError::InvalidResponse(
            "response request identifier did not match".to_owned(),
        ));
    }
    Ok(match response {
        LocalApprovalResponse::Decision { decision, .. } => ApprovalClientOutcome {
            presented: true,
            decision: Some(decision),
            answer: None,
        },
        LocalApprovalResponse::Answer { option_index, .. } => ApprovalClientOutcome {
            presented: true,
            decision: None,
            answer: Some(option_index),
        },
        LocalApprovalResponse::Unavailable { presented, .. } => ApprovalClientOutcome {
            presented,
            decision: None,
            answer: None,
        },
    })
}

#[cfg(not(unix))]
pub fn request(
    _paths: &StatePaths,
    _request: &LocalApprovalRequest,
) -> Result<ApprovalClientOutcome, ApprovalClientError> {
    Ok(ApprovalClientOutcome::default())
}
