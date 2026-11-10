//! Shared archive admission for native attachment export.
use super::{NodeError, invalid};
use base64::Engine;
const MAX_BYTES: usize = 64 * 1024 * 1024;

pub(super) fn attachment(bytes: &[u8], index: usize) -> Result<Vec<u8>, NodeError> {
    inspect(bytes, Some(index))?.ok_or_else(|| invalid("Record has no attachment"))
}

pub(super) fn validate(bytes: &[u8]) -> Result<(), NodeError> {
    inspect(bytes, None).map(|_| ())
}

fn inspect(bytes: &[u8], index: Option<usize>) -> Result<Option<Vec<u8>>, NodeError> {
    if bytes.len() > MAX_BYTES {
        return Err(invalid("Archive exceeds budget"));
    }
    let record: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| invalid("Invalid chat record"))?;
    let object = record
        .as_object()
        .ok_or_else(|| invalid("Invalid chat record"))?;
    if object.get("format").and_then(serde_json::Value::as_str) != Some("MTCHAT1") {
        return Err(invalid("Unsupported chat record"));
    }
    string(&record["title"], 512)?;
    timestamp(&record["created_at_ms"])?;
    let messages = record["messages"]
        .as_array()
        .filter(|rows| !rows.is_empty() && rows.len() <= 100)
        .ok_or_else(|| invalid("Invalid record count"))?;
    let mut output = None;
    let mut total = 0usize;
    for (position, message) in messages.iter().enumerate() {
        if !message.is_object()
            || !message["protection"].is_null()
            || !message["expires_at_ms"].is_null()
        {
            return Err(invalid("Unsupported record policy"));
        }
        string(&message["sender_name"], 256)?;
        string(&message["text"], 256 * 1024)?;
        timestamp(&message["wall_clock"])?;
        let file = &message["file"];
        if file.is_null() {
            continue;
        }
        if !file.is_object() {
            return Err(invalid("Invalid attachment"));
        }
        let name = string(&file["name"], 255)?;
        let mime = string(&file["mime"], 255)?;
        if name.is_empty()
            || name == "."
            || name == ".."
            || name
                .chars()
                .any(|c| c == '/' || c == '\\' || (c as u32) < 32)
            || name.to_ascii_lowercase().ends_with(".mtchat")
            || mime == "application/vnd.mesh-talk.chat+json"
        {
            return Err(invalid("Invalid nested attachment"));
        }
        let size = file["size"]
            .as_u64()
            .filter(|n| *n <= MAX_BYTES as u64)
            .ok_or_else(|| invalid("Invalid attachment size"))?;
        let encoded = string(&file["data_base64"], MAX_BYTES)?;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .map_err(|_| invalid("Invalid attachment data"))?;
        if decoded.len() as u64 != size
            || base64::engine::general_purpose::STANDARD.encode(&decoded) != encoded
        {
            return Err(invalid("Attachment size mismatch"));
        }
        total = total
            .checked_add(decoded.len())
            .filter(|n| *n <= MAX_BYTES)
            .ok_or_else(|| invalid("Attachment budget exceeded"))?;
        if Some(position) == index {
            output = Some(decoded);
        }
    }
    Ok(output)
}
fn string(value: &serde_json::Value, max: usize) -> Result<&str, NodeError> {
    value
        .as_str()
        .filter(|s| s.len() <= max)
        .ok_or_else(|| invalid("Invalid record text"))
}
fn timestamp(value: &serde_json::Value) -> Result<(), NodeError> {
    value
        .as_u64()
        .filter(|n| *n <= 8_640_000_000_000_000)
        .map(|_| ())
        .ok_or_else(|| invalid("Invalid record timestamp"))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture() -> serde_json::Value {
        serde_json::json!({"format":"MTCHAT1","title":"聊天记录","created_at_ms":1,"messages":[{"sender_name":"甲","wall_clock":1,"text":"hello","file":{"name":"file.bin","mime":"application/octet-stream","size":3,"data_base64":"YWJj"}}]})
    }
    fn decode(value: &serde_json::Value) -> Result<Vec<u8>, NodeError> {
        attachment(&serde_json::to_vec(value).unwrap(), 0)
    }
    #[test]
    fn exports_exact_contained_bytes_and_rejects_unavailable_index() {
        let value = fixture();
        assert_eq!(decode(&value).unwrap(), b"abc");
        assert!(attachment(&serde_json::to_vec(&value).unwrap(), 1).is_err());
    }
    #[test]
    fn rejects_policies_paths_nested_mime_and_malformed_metadata() {
        for field in ["protection", "expires_at_ms"] {
            let mut value = fixture();
            value["messages"][0][field] = serde_json::json!(1);
            assert!(decode(&value).is_err());
        }
        for name in ["../file", "a\\b", ".", "..", "nested.MTCHAT", "bad\nname"] {
            let mut value = fixture();
            value["messages"][0]["file"]["name"] = serde_json::json!(name);
            assert!(decode(&value).is_err());
        }
        let mut value = fixture();
        value["messages"][0]["file"]["mime"] =
            serde_json::json!("application/vnd.mesh-talk.chat+json");
        assert!(decode(&value).is_err());
        for timestamp in [
            serde_json::json!(-1),
            serde_json::json!(1.5),
            serde_json::json!(8_640_000_000_000_001u64),
        ] {
            let mut value = fixture();
            value["created_at_ms"] = timestamp;
            assert!(decode(&value).is_err());
        }
    }
    #[test]
    fn bounds_unicode_by_utf8_bytes_and_checks_every_attachment() {
        let mut value = fixture();
        value["messages"][0]["sender_name"] = serde_json::json!("甲".repeat(100));
        assert!(decode(&value).is_err());
        let mut value = fixture();
        value["messages"].as_array_mut().unwrap().push(serde_json::json!({"sender_name":"B","wall_clock":1,"text":"x","file":{"name":"bad","mime":"x","size":4,"data_base64":"YWJj"}}));
        assert!(decode(&value).is_err());
    }
}
