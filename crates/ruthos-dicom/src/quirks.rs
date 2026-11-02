//! Vendor quirks live here and nowhere else.
//!
//! Ruthos relies on strict, standards-based decoding. When a specific
//! manufacturer or model writes something the standard does not allow, the
//! adjustment is expressed as a [`Quirk`] that inspects [`InstanceMeta`] and
//! patches it. Every quirk gets its own test file. Feature code never checks a
//! manufacturer string directly.

use crate::meta::InstanceMeta;

/// One vendor-specific adjustment.
pub trait Quirk: Send + Sync {
    /// Short identifier, used in logs.
    fn id(&self) -> &'static str;
    /// Whether this quirk applies to the given header.
    fn applies(&self, meta: &InstanceMeta) -> bool;
    /// Patch the header in place.
    fn apply(&self, meta: &mut InstanceMeta);
}

/// The active set of quirks.
#[derive(Default)]
pub struct QuirkRegistry {
    quirks: Vec<Box<dyn Quirk>>,
}

impl QuirkRegistry {
    pub fn new() -> Self {
        Self { quirks: Vec::new() }
    }

    pub fn register(&mut self, quirk: Box<dyn Quirk>) {
        self.quirks.push(quirk);
    }

    /// Apply every matching quirk and return the ids of those applied.
    pub fn apply_all(&self, meta: &mut InstanceMeta) -> Vec<&'static str> {
        let mut applied = Vec::new();
        for q in &self.quirks {
            if q.applies(meta) {
                q.apply(meta);
                applied.push(q.id());
            }
        }
        applied
    }

    pub fn is_empty(&self) -> bool {
        self.quirks.is_empty()
    }
}
