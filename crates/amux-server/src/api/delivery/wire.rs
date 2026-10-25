//! Version-one Claude peer framing. The stamped body is data inside one envelope.
use super::{valid_uuid, ReceiptState, MAX_WIRE_BYTES, WIRE_VERSION};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub(super) fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

pub(super) fn address(path: &std::path::Path) -> Option<String> {
    let path = path.to_str()?;
    let mut s = String::from("uds:");
    for b in path.bytes() {
        if b.is_ascii_alphanumeric() || b":_/.-\\".contains(&b) {
            s.push(b as char);
        } else {
            s.push_str(&format!("%{b:02X}"));
        }
    }
    Some(s)
}

fn name(s: &str) -> String {
    static UNSAFE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    // Rust strings contain Unicode scalar values and cannot contain surrogates.
    let re = UNSAFE.get_or_init(|| regex::Regex::new(r#"[\p{Cc}\p{Cf}\p{Zl}\p{Zp}"<>]"#).unwrap());
    let s = re.replace_all(s, "");
    let s = s.trim();
    let mut out: String = s.chars().take(super::MAX_SENDER_NAME_CHARS).collect();
    if s.chars().count() > super::MAX_SENDER_NAME_CHARS {
        out.push('…');
    }
    out
}

pub(super) fn envelope(from: &str, sender: &str, mode: Option<&str>, stamped: &str) -> String {
    let sender = name(sender);
    let mut attrs = format!(" from=\"{from}\"");
    if !sender.is_empty() {
        attrs.push_str(&format!(" from-name=\"{sender}\""));
    }
    if let Some(mode @ ("prompting" | "bypass")) = mode {
        attrs.push_str(&format!(" from-mode=\"{mode}\""));
    }
    let body = neutralize(stamped);
    format!("<cross-session-message{attrs}>\n{body}\n</cross-session-message>")
}

/// The reviewed serializer recognizes invisible separators, Unicode dash/
/// slash/angle lookalikes and nested wrappers. Escape the opening delimiter
/// of wrapper-shaped text; unrelated HTML/code remains byte-exact.
pub(super) fn neutralize(body: &str) -> String {
    static TAG: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    let tag=TAG.get_or_init(|| {
        let open="<＜﹤〈⟨〈‹˂ᐸ❬❮❰⧼≮≺⋖";
        let close=">＞﹥〉⟩〉›˃ᐳ❭❯❱⧽≯≻⋗";
        let filler=format!("[^A-Za-z0-9_\\-{open}{close}]*");
        let invisible=r"[\p{M}\p{Cf}\p{Cc}\p{Zl}\p{Zp}\u{115f}\u{1160}\u{3164}\u{ffa0}]*";
        let spelling="cross-session-message".chars().map(|c| {
            if c=='-' {r"[_\p{Pc}\p{Pd}\u{2017}\u{02cd}\u{07fa}\u{0640}\u{2212}\u{207b}\u{208b}\u{02d7}\u{2796}\u{2043}\u{30fc}\u{ff70}]".to_string()}
            else {c.to_string()}
        }).collect::<Vec<_>>().join(invisible);
        regex::Regex::new(&format!("(?i)[{open}]({filler}[/／∕⁄]?{filler}{spelling})([^A-Za-z0-9_\\-]|$)")).unwrap()
    });
    tag.replace_all(body, |c: &regex::Captures<'_>| {
        if c[1].starts_with('\\') {
            c[0].to_string()
        } else {
            format!("<\\{}{}", &c[1], &c[2])
        }
    })
    .into_owned()
}

pub(super) struct Frame<'a> {
    pub from: &'a str,
    pub sender: &'a str,
    pub mode: Option<&'a str>,
    pub body: &'a str,
    pub msg_id: &'a str,
    pub command_uuid: &'a str,
    pub session_id: &'a str,
    pub token: Option<&'a str>,
}

pub(super) fn encode(f: &Frame<'_>) -> Result<Vec<u8>, &'static str> {
    if !valid_uuid(f.msg_id) || !valid_uuid(f.command_uuid) {
        return Err("invalid_uuid");
    }
    let content = envelope(f.from, f.sender, f.mode, f.body);
    let user = json!({"type":"user", "message":{"role":"user","content":content},
        "priority":"next", "from":f.from, "msgV":WIRE_VERSION, "msg_id":f.msg_id,
        "uuid":f.command_uuid, "session_id":f.session_id});
    let mut bytes = Vec::new();
    if let Some(token) = f.token {
        if token.len() != super::AUTH_TOKEN_HEX_CHARS
            || !token.bytes().all(|b| b.is_ascii_hexdigit())
        {
            return Err("invalid_auth_token");
        }
        serde_json::to_writer(&mut bytes, &json!({"type":"auth","token":token}))
            .map_err(|_| "encode_auth")?;
        bytes.push(b'\n');
    }
    serde_json::to_writer(&mut bytes, &user).map_err(|_| "encode_user")?;
    bytes.push(b'\n');
    if bytes.len() > MAX_WIRE_BYTES {
        return Err("payload_byte_cap");
    }
    Ok(bytes)
}

/// Reverse status is not an ordinary positive ACK. Delivered reports a held
/// release (possibly arriving before held), proving admission, never consumption.
pub(super) fn callback(v: &Value) -> Result<(ReceiptState, Vec<String>), &'static str> {
    if v["type"] != "control"
        || v["action"] != "peer_message_status"
        || v["msgV"] != WIRE_VERSION
        || !v["msg_id"].as_str().is_some_and(valid_uuid)
    {
        return Err("callback_protocol");
    }
    let state = match v["status"].as_str() {
        Some("held") => ReceiptState::Held,
        Some("delivered") => ReceiptState::Admitted,
        Some("denied") => ReceiptState::Denied,
        Some("refused") => ReceiptState::Refused,
        Some("expired") if v["status_detail"] == "refused" => ReceiptState::Refused,
        Some("expired") => ReceiptState::Expired,
        Some("dropped") => ReceiptState::Dropped,
        _ => return Err("callback_status"),
    };
    callback_reason(v)?;
    let mut ids = Vec::new();
    if let Some(s) = v["orig_msg_id"].as_str() {
        if !valid_uuid(s) {
            return Err("callback_orig_id");
        }
        ids.push(s.to_string());
    }
    if let Some(dropped) = v.get("dropped_msg_ids") {
        let dropped = dropped.as_array().ok_or("callback_dropped_ids")?;
        if state != ReceiptState::Dropped || dropped.len() > super::MAX_CALLBACK_IDS {
            return Err("callback_dropped_ids");
        }
        for id in dropped {
            let id = id
                .as_str()
                .filter(|s| valid_uuid(s))
                .ok_or("callback_dropped_id")?;
            if !ids.iter().any(|s| s == id) {
                ids.push(id.to_string());
            }
        }
    }
    if ids.is_empty() {
        return Err("callback_missing_id");
    }
    Ok((state, ids))
}

pub(super) fn callback_reason(v: &Value) -> Result<&'static str, &'static str> {
    const DROP_REASONS: &[&str] = &[
        "rate-limited",
        "duplicate",
        "hop-loop",
        "hop-runaway",
        "queue-full",
    ];
    match v.get("drop_reason") {
        None => Ok("verified_callback"),
        Some(Value::String(reason)) if v["status"] == "dropped" => DROP_REASONS
            .iter()
            .copied()
            .find(|known| *known == reason.as_str())
            .ok_or("callback_drop_reason"),
        _ => Err("callback_drop_reason"),
    }
}
