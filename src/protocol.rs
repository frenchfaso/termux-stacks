use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::fmt;
use std::io::{self, BufRead, Write};

pub(crate) const VERSION: u32 = 1;
pub(crate) const MAX_FRAME_BYTES: usize = 1024 * 1024;

#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "command", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum Request {
    Up {
        protocol_version: u32,
        request_id: String,
        manifest: String,
    },
    Status {
        protocol_version: u32,
        request_id: String,
        stack: String,
    },
    Down {
        protocol_version: u32,
        request_id: String,
        stack: String,
    },
}

impl Request {
    pub(crate) fn protocol_version(&self) -> u32 {
        match self {
            Self::Up {
                protocol_version, ..
            }
            | Self::Status {
                protocol_version, ..
            }
            | Self::Down {
                protocol_version, ..
            } => *protocol_version,
        }
    }

    pub(crate) fn request_id(&self) -> &str {
        match self {
            Self::Up { request_id, .. }
            | Self::Status { request_id, .. }
            | Self::Down { request_id, .. } => request_id,
        }
    }

    pub(crate) fn validate_envelope(&self) -> Result<(), ProtocolError> {
        if self.protocol_version() != VERSION {
            return Err(ProtocolError::Version(self.protocol_version()));
        }
        let id = self.request_id();
        if id.is_empty()
            || id.len() > 128
            || !id
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        {
            return Err(ProtocolError::InvalidRequestId);
        }
        Ok(())
    }
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Response {
    pub(crate) protocol_version: u32,
    pub(crate) request_id: String,
    pub(crate) ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) error: Option<ResponseError>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResponseError {
    pub(crate) code: String,
    pub(crate) message: String,
}

impl Response {
    pub(crate) fn success(request_id: impl Into<String>, result: Value) -> Self {
        Self {
            protocol_version: VERSION,
            request_id: request_id.into(),
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    pub(crate) fn failure(
        request_id: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            protocol_version: VERSION,
            request_id: request_id.into(),
            ok: false,
            result: None,
            error: Some(ResponseError {
                code: code.into(),
                message: message.into(),
            }),
        }
    }

    pub(crate) fn validate(&self, request_id: &str) -> Result<(), ProtocolError> {
        if self.protocol_version != VERSION {
            return Err(ProtocolError::Version(self.protocol_version));
        }
        if self.request_id != request_id {
            return Err(ProtocolError::MismatchedRequestId);
        }
        if self.ok != self.result.is_some() || self.ok == self.error.is_some() {
            return Err(ProtocolError::InvalidResponse);
        }
        Ok(())
    }
}

#[derive(Debug)]
pub(crate) enum ProtocolError {
    Io(io::Error),
    MissingNewline,
    FrameTooLarge,
    InvalidJson(serde_json::Error),
    Version(u32),
    InvalidRequestId,
    MismatchedRequestId,
    InvalidResponse,
}

impl fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "control socket I/O failed: {error}"),
            Self::MissingNewline => formatter.write_str("control frame is not newline terminated"),
            Self::FrameTooLarge => write!(
                formatter,
                "control frame exceeds the {MAX_FRAME_BYTES}-byte limit"
            ),
            Self::InvalidJson(error) => write!(formatter, "invalid control frame: {error}"),
            Self::Version(version) => write!(
                formatter,
                "unsupported protocol version {version}; expected {VERSION}"
            ),
            Self::InvalidRequestId => formatter.write_str("invalid request_id"),
            Self::MismatchedRequestId => formatter.write_str("response request_id does not match"),
            Self::InvalidResponse => formatter.write_str("invalid response envelope"),
        }
    }
}

impl From<io::Error> for ProtocolError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

pub(crate) fn read_request(reader: &mut impl BufRead) -> Result<Option<Request>, ProtocolError> {
    read_json_line(reader)
}

pub(crate) fn read_response(reader: &mut impl BufRead) -> Result<Option<Response>, ProtocolError> {
    read_json_line(reader)
}

fn read_json_line<T>(reader: &mut impl BufRead) -> Result<Option<T>, ProtocolError>
where
    T: for<'de> Deserialize<'de>,
{
    let mut frame = Vec::new();
    let mut limited = std::io::Read::take(&mut *reader, (MAX_FRAME_BYTES + 2) as u64);
    let count = limited.read_until(b'\n', &mut frame)?;
    if count == 0 {
        return Ok(None);
    }
    if frame.last() != Some(&b'\n') {
        return Err(if frame.len() > MAX_FRAME_BYTES {
            ProtocolError::FrameTooLarge
        } else {
            ProtocolError::MissingNewline
        });
    }
    frame.pop();
    if frame.len() > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge);
    }
    serde_json::from_slice(&frame)
        .map(Some)
        .map_err(ProtocolError::InvalidJson)
}

pub(crate) fn write_frame<T>(writer: &mut impl Write, value: &T) -> Result<(), ProtocolError>
where
    T: Serialize,
{
    let frame = serde_json::to_vec(value).map_err(ProtocolError::InvalidJson)?;
    if frame.len() > MAX_FRAME_BYTES {
        return Err(ProtocolError::FrameTooLarge);
    }
    writer.write_all(&frame)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{ProtocolError, Request, Response, VERSION, read_request, write_frame};
    use serde_json::json;
    use std::io::Cursor;

    #[test]
    fn request_round_trip_is_newline_delimited() {
        let request = Request::Status {
            protocol_version: VERSION,
            request_id: "request-1".into(),
            stack: "hello".into(),
        };
        let mut bytes = Vec::new();
        write_frame(&mut bytes, &request).expect("write frame");
        assert_eq!(bytes.last(), Some(&b'\n'));
        let decoded = read_request(&mut Cursor::new(bytes))
            .expect("read frame")
            .expect("request");
        assert_eq!(decoded.request_id(), "request-1");
        decoded.validate_envelope().expect("valid envelope");
    }

    #[test]
    fn response_requires_one_result_shape() {
        let response = Response::success("one", json!({"state": "absent"}));
        response.validate("one").expect("valid response");
        assert!(matches!(
            response.validate("two"),
            Err(ProtocolError::MismatchedRequestId)
        ));
    }

    #[test]
    fn rejects_a_frame_without_newline() {
        let error = read_request(&mut Cursor::new(br#"{"command":"status"}"#))
            .expect_err("newline is required");
        assert!(matches!(error, ProtocolError::MissingNewline));
    }

    #[test]
    fn rejects_unknown_request_fields() {
        let bytes = br#"{"command":"status","protocol_version":1,"request_id":"one","stack":"hello","extra":true}
"#;
        assert!(read_request(&mut Cursor::new(bytes)).is_err());
    }
}
