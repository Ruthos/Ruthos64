//! The relay's protocol, version 1: the envelope, close codes and the shared
//! state schema. The relay stores the shared state as JSON and checks only
//! its size and numbers, so Ruthos owns what it means.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const PROTOCOL: u32 = 1;
pub const SCHEMA: u64 = 1;
/// The relay's limit on a frame.
pub const MAX_FRAME: usize = 16 * 1024;
/// The relay's limit on one `set`.
pub const MAX_SET: usize = 8 * 1024;

/// Every message, in both directions.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub v: u32,
    pub t: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cid: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub from: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ts: Option<f64>,
    #[serde(default)]
    pub body: Value,
}

impl Envelope {
    pub fn new(t: &str, body: Value) -> Self {
        Self {
            v: PROTOCOL,
            t: t.to_string(),
            body,
            ..Default::default()
        }
    }
}

/// Closes after which the client never reconnects by itself.
pub fn close_is_final(code: u16) -> bool {
    (4000..=4004).contains(&code)
}

/// What the user is told about a final close.
pub fn close_reason(code: u16) -> &'static str {
    match code {
        4000 => "Session ended",
        4001 => "You were removed from the session, or the host did not admit you",
        4002 => "Ruthos needs updating to join this server",
        4003 => "Sign in again",
        4004 => "Disconnected for sending too fast. This is a fault: please report it",
        _ => "The connection closed",
    }
}

/// The confirmed shared state. Held as JSON so that fields added within
/// schema 1 by a newer client survive here unread.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SharedModel {
    pub seq: u64,
    pub layout: Map<String, Value>,
    pub cells: BTreeMap<u8, Map<String, Value>>,
    pub measurements: BTreeMap<String, Map<String, Value>>,
    /// Answer-key measurements revealed so far.
    pub revealed: Vec<String>,
}

impl SharedModel {
    /// The schema version, once a layout has been set.
    pub fn schema(&self) -> Option<u64> {
        self.layout.get("schema").and_then(Value::as_u64)
    }

    /// True when this client can draw the state.
    pub fn understood(&self) -> bool {
        self.schema().is_none_or(|s| s == SCHEMA)
    }

    /// A full `state` message. The state may be nested under `state` or sent
    /// directly in the body.
    pub fn from_state(seq: u64, body: &Value) -> Self {
        let s = body.get("state").unwrap_or(body);
        let object = |key: &str| s.get(key).and_then(Value::as_object).cloned().unwrap_or_default();
        let mut model = Self {
            seq,
            layout: object("layout"),
            ..Default::default()
        };
        for (k, v) in object("cells") {
            if let (Ok(n), Some(cell)) = (k.parse::<u8>(), v.as_object()) {
                model.cells.insert(n, cell.clone());
            }
        }
        for (k, v) in object("measurements").into_iter().chain(object("meas")) {
            if let Some(m) = v.as_object() {
                model.measurements.insert(k, m.clone());
            }
        }
        model
    }

    /// Apply a sequenced `change`.
    pub fn apply_change(&mut self, body: &Value) {
        let set = body.get("set").and_then(Value::as_object);
        match body.get("el").and_then(Value::as_str) {
            Some("layout") => {
                if let Some(set) = set {
                    merge(&mut self.layout, set);
                }
            }
            Some("cell") => {
                let n = body.get("cell").and_then(Value::as_u64);
                if let (Some(n @ 0..=63), Some(set)) = (n, set) {
                    merge(self.cells.entry(n as u8).or_default(), set);
                }
            }
            Some("meas") => {
                let Some(id) = body.get("id").and_then(Value::as_str) else {
                    return;
                };
                if body.get("op").and_then(Value::as_str) == Some("delete") {
                    self.measurements.remove(id);
                } else if let Some(set) = set {
                    merge(self.measurements.entry(id.to_string()).or_default(), set);
                }
            }
            _ => {}
        }
    }
}

/// Merge `set` into `into`; a null deletes the key.
fn merge(into: &mut Map<String, Value>, set: &Map<String, Value>) {
    for (k, v) in set {
        if v.is_null() {
            into.remove(k);
        } else {
            into.insert(k.clone(), v.clone());
        }
    }
}

/// A change the controller makes to the shared state.
#[derive(Debug, Clone, PartialEq)]
pub enum Change {
    Layout(Map<String, Value>),
    Cell { cell: u8, set: Map<String, Value> },
    MeasUpsert { id: String, set: Map<String, Value> },
    MeasDelete { id: String },
}

impl Change {
    pub fn body(&self) -> Value {
        match self {
            Change::Layout(set) => serde_json::json!({"el": "layout", "set": set}),
            Change::Cell { cell, set } => serde_json::json!({"el": "cell", "cell": cell, "set": set}),
            Change::MeasUpsert { id, set } => serde_json::json!({"el": "meas", "id": id, "set": set, "op": "upsert"}),
            Change::MeasDelete { id } => serde_json::json!({"el": "meas", "id": id, "op": "delete"}),
        }
    }
}

/// A member, as presence reports them. A participant's presence carries no
/// email addresses.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Member {
    #[serde(alias = "acct")]
    pub sub: String,
    pub name: String,
    pub online: bool,
    #[serde(alias = "follow")]
    pub following: bool,
    #[serde(alias = "hand_raised", alias = "raised")]
    pub hand: bool,
    pub admitted: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Presence {
    pub members: Vec<Member>,
    pub control: Option<String>,
    pub locked: bool,
    pub waiting: Vec<Member>,
}

/// The account in `welcome`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct WelcomeAccount {
    pub sub: String,
    pub name: String,
    pub host: bool,
    pub guest: bool,
}

/// Round to `places` decimals: 1 for window values, 2 for image points.
pub fn round(v: f32, places: i32) -> f64 {
    let m = 10f64.powi(places);
    (v as f64 * m).round() / m
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_null_deletes_and_unknown_fields_survive() {
        let mut m = SharedModel::default();
        m.apply_change(&json!({"el":"layout","set":{"schema":1,"rows":2,"future":"x"}}));
        m.apply_change(&json!({"el":"layout","set":{"rows":null}}));
        assert_eq!(m.layout.get("rows"), None);
        assert_eq!(m.layout["future"], "x");
        assert!(m.understood());
    }

    #[test]
    fn a_newer_schema_is_not_drawn() {
        let mut m = SharedModel::default();
        m.apply_change(&json!({"el":"layout","set":{"schema":2}}));
        assert!(!m.understood());
    }

    #[test]
    fn cells_past_63_are_ignored() {
        let mut m = SharedModel::default();
        m.apply_change(&json!({"el":"cell","cell":64,"set":{"frame":1}}));
        m.apply_change(&json!({"el":"cell","cell":63,"set":{"frame":1}}));
        assert_eq!(m.cells.len(), 1);
    }

    #[test]
    fn measurements_upsert_and_delete() {
        let mut m = SharedModel::default();
        m.apply_change(&json!({"el":"meas","id":"a","set":{"kind":"length"},"op":"upsert"}));
        m.apply_change(&json!({"el":"meas","id":"a","set":{"value":3.0},"op":"upsert"}));
        assert_eq!(m.measurements["a"]["kind"], "length");
        m.apply_change(&json!({"el":"meas","id":"a","op":"delete"}));
        assert!(m.measurements.is_empty());
    }

    #[test]
    fn close_codes() {
        assert!(close_is_final(4000) && close_is_final(4004));
        assert!(!close_is_final(1006) && !close_is_final(4005));
    }
}
