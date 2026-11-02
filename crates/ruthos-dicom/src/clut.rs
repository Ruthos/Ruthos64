//! Colour lookup tables.
//!
//! The tables are Horos's own CLUT definitions, embedded from
//! `assets/horos/cluts/*.plist` (LGPL-3.0, from the Horos project). Each is a
//! property list with three 256-entry integer arrays named Red, Green, Blue.

use std::io::Cursor;

/// A 256-entry RGB lookup table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clut {
    pub name: String,
    /// 256 entries of [r, g, b].
    pub table: Vec<[u8; 3]>,
}

impl Clut {
    /// The identity grayscale table Horos calls "No CLUT".
    pub fn grayscale() -> Self {
        Self {
            name: "No CLUT".to_string(),
            table: (0..=255u8).map(|v| [v, v, v]).collect(),
        }
    }

    /// Parse a Horos CLUT plist.
    pub fn from_plist_bytes(name: &str, bytes: &[u8]) -> Option<Self> {
        let value = plist::Value::from_reader(Cursor::new(bytes)).ok()?;
        let dict = value.as_dictionary()?;
        let channel = |key: &str| -> Option<Vec<u8>> {
            let arr = dict.get(key)?.as_array()?;
            let v: Vec<u8> = arr
                .iter()
                .map(|x| {
                    x.as_signed_integer()
                        .or_else(|| x.as_unsigned_integer().map(|u| u as i64))
                        .or_else(|| x.as_real().map(|r| r as i64))
                        .unwrap_or(0)
                        .clamp(0, 255) as u8
                })
                .collect();
            Some(v)
        };
        let r = channel("Red")?;
        let g = channel("Green")?;
        let b = channel("Blue")?;
        if r.len() < 256 || g.len() < 256 || b.len() < 256 {
            return None;
        }
        let table = (0..256).map(|i| [r[i], g[i], b[i]]).collect();
        Some(Self {
            name: name.to_string(),
            table,
        })
    }

    /// Table as a 256x1 RGBA8 texture payload.
    pub fn to_rgba_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(256 * 4);
        for [r, g, b] in &self.table {
            out.extend_from_slice(&[*r, *g, *b, 255]);
        }
        out
    }
}

macro_rules! embedded_cluts {
    ($($name:literal),* $(,)?) => {
        &[
            $(($name, include_bytes!(concat!("../../../assets/horos/cluts/", $name, ".plist")) as &[u8]),)*
        ]
    };
}

/// (name, plist bytes) for every CLUT shipped with Horos.
pub const EMBEDDED: &[(&str, &[u8])] = embedded_cluts![
    "BlackBody",
    "Flow",
    "French",
    "GEcolor",
    "GrayRainbow",
    "HotGreen",
    "HotIron",
    "HotMetal",
    "Hue1",
    "Hue2",
    "Jet",
    "NIH",
    "PPU Inferno",
    "PPU Magma",
    "PPU Plasma",
    "PPU Viridis",
    "Perfusion",
    "Rainbow",
    "Rainbow2",
    "Rainbow3",
    "Ratio",
    "Spectrum",
    "Stern",
    "UCLA",
    "VR Bones",
    "VR Muscles-Bones",
    "VR Red Vessels",
    "ired",
];

/// All known CLUTs, grayscale first.
#[derive(Debug, Clone)]
pub struct ClutLibrary {
    pub cluts: Vec<Clut>,
}

impl Default for ClutLibrary {
    fn default() -> Self {
        Self::load()
    }
}

impl ClutLibrary {
    pub fn load() -> Self {
        let mut cluts = vec![Clut::grayscale()];
        for (name, bytes) in EMBEDDED {
            if let Some(c) = Clut::from_plist_bytes(name, bytes) {
                cluts.push(c);
            } else {
                tracing::warn!("CLUT {name} could not be parsed");
            }
        }
        Self { cluts }
    }

    pub fn get(&self, name: &str) -> Option<&Clut> {
        self.cluts.iter().find(|c| c.name == name)
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.cluts.iter().map(|c| c.name.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_embedded_cluts_parse() {
        let lib = ClutLibrary::load();
        assert_eq!(lib.cluts.len(), EMBEDDED.len() + 1);
        let hot_iron = lib.get("HotIron").unwrap();
        assert_eq!(hot_iron.table.len(), 256);
        assert_eq!(hot_iron.table[0], [0, 0, 0]);
        assert_eq!(hot_iron.table[255], [255, 255, 255]);
    }

    #[test]
    fn grayscale_is_identity() {
        let g = Clut::grayscale();
        assert_eq!(g.table[128], [128, 128, 128]);
        assert_eq!(g.to_rgba_bytes().len(), 1024);
    }
}
