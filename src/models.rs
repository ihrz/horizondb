use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Deserialize)]
pub struct PacketMessage {
    pub id: String,
    pub operation: String,
    pub table: Option<String>,
    pub key: Option<String>,
    pub value: Option<Value>,
    pub amount: Option<f64>,
    pub element: Option<Value>,
    #[serde(rename = "sessionId")]
    pub session_id: Option<String>,
    pub login: Option<String>,
    pub password: Option<String>,
    #[serde(rename = "defaultValue")]
    pub default_value: Option<Value>,
    pub time: Option<u64>,
    /// `push`/`pull`: when true, the response carries the resulting array
    /// under `value` (saves the follow-up `get` a client would issue).
    #[serde(rename = "returnValue")]
    pub return_value: Option<bool>,
    /// Client-side timeout of this request in milliseconds. A request that
    /// waited longer than this before being handled is answered with an
    /// error instead of being executed for a caller that already gave up.
    #[serde(rename = "timeoutMs")]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct ResponseMessage {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: &'static str,
    pub data: Option<Value>,
    pub error: Option<String>,
}

impl ResponseMessage {
    pub fn success(id: String, data: Value) -> Self {
        Self {
            id,
            kind: "response",
            data: Some(data),
            error: None,
        }
    }

    pub fn failure(id: String, message: impl Into<String>) -> Self {
        Self {
            id,
            kind: "error",
            data: None,
            error: Some(message.into()),
        }
    }

    pub fn encode(&self) -> String {
        serde_json::to_string(self).unwrap_or_else(|_| {
            r#"{"id":"","type":"error","error":"internal serialization failure"}"#.to_owned()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn packet_deserializes_camel_case_fields() {
        let raw = r#"{"id":"p1","operation":"get","table":"t","key":"a.b","sessionId":"s","defaultValue":42}"#;
        let packet: PacketMessage = serde_json::from_str(raw).unwrap();
        assert_eq!(packet.id, "p1");
        assert_eq!(packet.session_id.as_deref(), Some("s"));
        assert_eq!(packet.default_value, Some(json!(42)));
        assert_eq!(packet.return_value, None);

        let raw = r#"{"id":"p2","operation":"push","key":"a","element":1,"returnValue":true,"timeoutMs":5000}"#;
        let packet: PacketMessage = serde_json::from_str(raw).unwrap();
        assert_eq!(packet.return_value, Some(true));
        assert_eq!(packet.timeout_ms, Some(5000));
    }

    #[test]
    fn response_keeps_null_fields_on_wire() {
        let encoded = ResponseMessage::success("7".to_owned(), Value::Null).encode();
        assert_eq!(
            encoded,
            r#"{"id":"7","type":"response","data":null,"error":null}"#
        );
    }

    #[test]
    fn failure_encodes_error_message() {
        let encoded = ResponseMessage::failure("9".to_owned(), "boom").encode();
        assert_eq!(
            encoded,
            r#"{"id":"9","type":"error","data":null,"error":"boom"}"#
        );
    }
}
