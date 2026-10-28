//! Frame helpers shared by server and client: every frame is one JSON object with
//! `"v":1` and a `"type"`.

use serde::Serialize;
use serde_json::{Map, Value, json};

pub const VERSION: u64 = 1;

/// Error codes after which the bridge must not reconnect.
pub const CODE_FENCED: &str = "fenced";
pub const CODE_HOST_MISMATCH: &str = "host_mismatch";
pub const CODE_BAD_HELLO: &str = "bad_hello";
/// No hello arrived in time: the connection is closed and the bridge reconnects.
pub const CODE_HELLO_TIMEOUT: &str = "hello_timeout";

/// Serializes `payload` (a struct or object) and adds `v` and `type`.
pub fn frame<T: Serialize>(kind: &str, payload: &T) -> String {
    let mut obj = match serde_json::to_value(payload) {
        Ok(Value::Object(m)) => m,
        _ => Map::new(),
    };
    obj.insert("v".into(), json!(VERSION));
    obj.insert("type".into(), json!(kind));
    Value::Object(obj).to_string()
}

pub fn error_frame(code: &str, message: &str) -> String {
    frame("error", &json!({"code": code, "message": message}))
}

/// A response object for a request that never reached the dispatcher.
pub fn error_response(request_id: Option<Value>, code: &str, message: &str) -> Value {
    json!({
        "v": VERSION,
        "type": "response",
        "request_id": request_id.unwrap_or(Value::Null),
        "ok": false,
        "error": {"code": code, "message": message},
    })
}

/// Parses one text frame into its object and type; rejects non-objects and other versions.
pub fn parse(text: &str) -> Result<(String, Value), String> {
    let value: Value = serde_json::from_str(text).map_err(|e| format!("invalid JSON: {e}"))?;
    let obj = value.as_object().ok_or("frame must be a JSON object")?;
    match obj.get("v").and_then(Value::as_u64) {
        Some(VERSION) => {}
        _ => {
            return Err(format!(
                "unsupported or missing frame version (want v={VERSION})"
            ));
        }
    }
    let kind = obj
        .get("type")
        .and_then(Value::as_str)
        .ok_or("frame has no type")?
        .to_owned();
    Ok((kind, value))
}

/// Resolves once the shutdown flag is set; never resolves if its sender is dropped first.
pub async fn shutdown_signal(rx: &mut tokio::sync::watch::Receiver<bool>) {
    if rx.wait_for(|s| *s).await.is_err() {
        std::future::pending::<()>().await;
    }
}
