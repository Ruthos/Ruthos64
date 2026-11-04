//! Sequence tracking, gap recovery, and the controller's outgoing coalescer.

use std::collections::{HashMap, VecDeque};

use serde_json::{json, Value};

use crate::proto::{Change, Envelope, SharedModel};

/// What the connection must do after a sequenced message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sequenced {
    /// In order: act on it.
    Apply,
    /// Already seen.
    Ignore,
    /// A gap: ask for what is missing after this sequence number.
    Request(u64),
    /// Being recovered: the reply covers it.
    Discard,
}

/// The shared model and where it stands in the relay's sequence.
#[derive(Debug, Default)]
pub struct Tracker {
    pub model: SharedModel,
    /// Waiting for the reply to a `state_request`.
    pub recovering: bool,
}

impl Tracker {
    pub fn last_seq(&self) -> u64 {
        self.model.seq
    }

    /// Place a sequenced message against `last_seq`. The relay's sequence
    /// rises by one, so anything further on means messages were lost.
    pub fn place(&mut self, seq: u64) -> Sequenced {
        let last = self.model.seq;
        if seq <= last {
            Sequenced::Ignore
        } else if seq == last + 1 {
            self.model.seq = seq;
            self.recovering = false;
            Sequenced::Apply
        } else if self.recovering {
            Sequenced::Discard
        } else {
            self.recovering = true;
            Sequenced::Request(last)
        }
    }

    /// A full state replaces the model.
    pub fn replace(&mut self, model: SharedModel) {
        self.model = model;
        self.recovering = false;
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Key {
    Layout,
    Cell(u8),
    Meas(String),
}

/// Keeps the latest value per element and sends elements in turn, so that
/// scrolling many cells stays within the relay's rate. `pointer` and `drag`
/// are latest-only and take at most every other slot, since the relay
/// forwards no more than 30 a second of them.
#[derive(Debug, Default)]
pub struct Coalescer {
    order: VecDeque<Key>,
    pending: HashMap<Key, Change>,
    pointer: Option<Value>,
    drags: VecDeque<((String, u16), Value)>,
    last_was_ephemeral: bool,
}

impl Coalescer {
    pub fn push(&mut self, change: Change) {
        let key = match &change {
            Change::Layout(_) => Key::Layout,
            Change::Cell { cell, .. } => Key::Cell(*cell),
            Change::MeasUpsert { id, .. } | Change::MeasDelete { id } => Key::Meas(id.clone()),
        };
        match (self.pending.get_mut(&key), change) {
            (Some(Change::Layout(old)), Change::Layout(new)) => old.extend(new),
            (Some(Change::Cell { set: old, .. }), Change::Cell { set: new, .. }) => old.extend(new),
            (Some(Change::MeasUpsert { set: old, .. }), Change::MeasUpsert { set: new, .. }) => old.extend(new),
            (Some(slot), change) => *slot = change,
            (None, change) => {
                self.pending.insert(key.clone(), change);
                self.order.push_back(key);
            }
        }
    }

    pub fn pointer(&mut self, body: Value) {
        self.pointer = Some(body);
    }

    pub fn drag(&mut self, id: String, i: u16, x: f32, y: f32) {
        let key = (id.clone(), i);
        let body = json!({"id": id, "i": i, "x": x, "y": y});
        match self.drags.iter_mut().find(|(k, _)| *k == key) {
            Some(slot) => slot.1 = body,
            None => self.drags.push_back((key, body)),
        }
    }

    /// The next message, for one send slot.
    pub fn take_next(&mut self) -> Option<Envelope> {
        let ephemeral = self.pointer.is_some() || !self.drags.is_empty();
        if ephemeral && (!self.last_was_ephemeral || self.order.is_empty()) {
            self.last_was_ephemeral = true;
            if let Some((_, body)) = self.drags.pop_front() {
                return Some(Envelope::new("drag", body));
            }
            return self.pointer.take().map(|b| Envelope::new("pointer", b));
        }
        self.last_was_ephemeral = false;
        let key = self.order.pop_front()?;
        let change = self.pending.remove(&key)?;
        Some(Envelope::new("change", change.body()))
    }

    pub fn clear(&mut self) {
        *self = Self::default();
    }
}

/// Remove points of a freehand path that lie within `tolerance` of the line
/// between their neighbours (Douglas-Peucker).
pub fn simplify(points: &[[f32; 2]], tolerance: f32) -> Vec<[f32; 2]> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let mut keep = vec![false; points.len()];
    keep[0] = true;
    keep[points.len() - 1] = true;
    let mut stack = vec![(0, points.len() - 1)];
    while let Some((a, b)) = stack.pop() {
        let mut worst = (0.0f32, 0);
        for (i, p) in points.iter().enumerate().take(b).skip(a + 1) {
            let d = distance_to_segment(*p, points[a], points[b]);
            if d > worst.0 {
                worst = (d, i);
            }
        }
        if worst.0 > tolerance {
            keep[worst.1] = true;
            stack.push((a, worst.1));
            stack.push((worst.1, b));
        }
    }
    points.iter().zip(keep).filter(|(_, k)| *k).map(|(p, _)| *p).collect()
}

fn distance_to_segment(p: [f32; 2], a: [f32; 2], b: [f32; 2]) -> f32 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let len = dx * dx + dy * dy;
    let t = if len > 0.0 {
        (((p[0] - a[0]) * dx + (p[1] - a[1]) * dy) / len).clamp(0.0, 1.0)
    } else {
        0.0
    };
    ((p[0] - a[0] - t * dx).powi(2) + (p[1] - a[1] - t * dy).powi(2)).sqrt()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Map;

    fn set(k: &str, v: i32) -> Map<String, Value> {
        let mut m = Map::new();
        m.insert(k.into(), v.into());
        m
    }

    #[test]
    fn a_gap_asks_once_and_discards_until_the_replay() {
        let mut t = Tracker::default();
        assert_eq!(t.place(1), Sequenced::Apply);
        assert_eq!(t.place(2), Sequenced::Apply);
        assert_eq!(t.place(4), Sequenced::Request(2));
        assert_eq!(t.place(5), Sequenced::Discard);
        assert_eq!(t.place(3), Sequenced::Apply);
        assert_eq!(t.place(4), Sequenced::Apply);
        assert_eq!(t.place(4), Sequenced::Ignore);
        assert_eq!(t.place(1), Sequenced::Ignore);
    }

    #[test]
    fn a_full_state_ends_recovery() {
        let mut t = Tracker::default();
        t.place(5);
        assert!(t.recovering);
        t.replace(SharedModel {
            seq: 9,
            ..Default::default()
        });
        assert!(!t.recovering);
        assert_eq!(t.place(10), Sequenced::Apply);
    }

    #[test]
    fn only_the_latest_value_of_a_cell_is_sent() {
        let mut c = Coalescer::default();
        for v in 0..100 {
            c.push(Change::Cell {
                cell: 3,
                set: set("frame", v),
            });
        }
        c.push(Change::Cell {
            cell: 3,
            set: set("wc", 7),
        });
        let e = c.take_next().unwrap();
        assert_eq!(e.body["set"]["frame"], 99);
        assert_eq!(e.body["set"]["wc"], 7);
        assert!(c.take_next().is_none());
    }

    #[test]
    fn elements_take_turns() {
        let mut c = Coalescer::default();
        for (cell, v) in [(0, 1), (1, 1), (0, 2)] {
            c.push(Change::Cell {
                cell,
                set: set("frame", v),
            });
        }
        assert_eq!(c.take_next().unwrap().body["cell"], 0);
        assert_eq!(c.take_next().unwrap().body["cell"], 1);
    }

    #[test]
    fn a_delete_replaces_a_waiting_upsert() {
        let mut c = Coalescer::default();
        c.push(Change::MeasUpsert {
            id: "a".into(),
            set: set("x", 1),
        });
        c.push(Change::MeasDelete { id: "a".into() });
        assert_eq!(c.take_next().unwrap().body["op"], "delete");
        assert!(c.take_next().is_none());
    }

    #[test]
    fn pointer_is_latest_only_and_shares_the_slots() {
        let mut c = Coalescer::default();
        c.pointer(json!({"x": 1}));
        c.pointer(json!({"x": 2}));
        c.push(Change::Cell {
            cell: 0,
            set: set("frame", 1),
        });
        let first = c.take_next().unwrap();
        assert_eq!((first.t.as_str(), first.body["x"].as_i64()), ("pointer", Some(2)));
        assert_eq!(c.take_next().unwrap().t, "change");
    }

    #[test]
    fn a_path_is_simplified_within_the_tolerance() {
        let line: Vec<[f32; 2]> = (0..1000).map(|i| [i as f32, (i % 2) as f32 * 0.1]).collect();
        assert_eq!(simplify(&line, 0.25).len(), 2);
        let corner = [[0.0, 0.0], [5.0, 0.0], [5.0, 5.0]];
        assert_eq!(simplify(&corner, 0.25).len(), 3);
    }
}
