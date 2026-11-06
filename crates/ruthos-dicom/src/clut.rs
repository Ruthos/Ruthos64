//! Colour lookup tables.
//!
//! The tables are Horos's own CLUT definitions, embedded from
//! `assets/horos/cluts/*.plist` (LGPL-3.0, from the Horos project). Each is a
//! property list with three 256-entry integer arrays named Red, Green, Blue.
//!
//! The CT maps are Ruthos's own. Their colours stand for fixed Hounsfield
//! ranges, so each carries the window it is true at.

use std::io::Cursor;

/// Where a table sits in the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClutGroup {
    Gray,
    Ct,
    Classic,
    Scientific,
    Rendering,
}

impl ClutGroup {
    fn of(name: &str) -> Self {
        match name {
            "PPU Inferno" | "PPU Magma" | "PPU Plasma" | "PPU Viridis" | "Flow" | "Perfusion" | "Ratio" => {
                ClutGroup::Scientific
            }
            "VR Bones" | "VR Muscles-Bones" | "VR Red Vessels" => ClutGroup::Rendering,
            _ => ClutGroup::Classic,
        }
    }
}

/// A 256-entry RGB lookup table.
#[derive(Debug, Clone, PartialEq)]
pub struct Clut {
    pub name: String,
    /// 256 entries of [r, g, b].
    pub table: Vec<[u8; 3]>,
    pub group: ClutGroup,
    /// The values the table runs from and to, when its colours stand for them.
    pub span: Option<(f32, f32)>,
    /// What the colours mean, for a map that has a meaning.
    pub about: &'static str,
}

/// How a band of values is coloured.
#[derive(Clone, Copy)]
enum Fill {
    Flat([u8; 3]),
    Ramp([u8; 3], [u8; 3]),
}

const BLACK: [u8; 3] = [0, 0, 0];
const WHITE: [u8; 3] = [255, 255, 255];
const GREY: [u8; 3] = [170, 170, 170];
const DARK: [u8; 3] = [64, 64, 64];
const YELLOW: [u8; 3] = [236, 196, 58];
const AMBER: [u8; 3] = [246, 146, 30];
const RED: [u8; 3] = [214, 40, 40];
const BLUE: [u8; 3] = [62, 112, 184];
const GREEN: [u8; 3] = [46, 160, 84];
const MAGENTA: [u8; 3] = [190, 32, 190];

/// A table from bands, each running up to a value. The last goes to the top.
fn banded(name: &str, span: (f32, f32), about: &'static str, bands: &[(f32, Fill)]) -> Clut {
    let (low, high) = span;
    let table = (0..256)
        .map(|i| {
            let value = low + (high - low) * i as f32 / 255.0;
            let at = bands
                .iter()
                .position(|(until, _)| value < *until)
                .unwrap_or(bands.len() - 1);
            match bands[at].1 {
                Fill::Flat(c) => c,
                Fill::Ramp(a, b) => {
                    let from = if at == 0 { low } else { bands[at - 1].0 };
                    let t = ((value - from) / (bands[at].0 - from)).clamp(0.0, 1.0);
                    std::array::from_fn(|k| (a[k] as f32 + (b[k] as f32 - a[k] as f32) * t).round() as u8)
                }
            }
        })
        .collect();
    Clut {
        name: name.to_string(),
        table,
        group: ClutGroup::Ct,
        span: Some(span),
        about,
    }
}

/// The maps for CT, in Hounsfield units.
fn ct_maps() -> Vec<Clut> {
    use Fill::{Flat, Ramp};
    const TOP: f32 = f32::MAX;
    vec![
        banded(
            "CT Tissues",
            (-250.0, 750.0),
            "Air below -190. Fat -190 to -30. Fluid -30 to 20. Soft tissue 20 to 100. \
             Cancellous bone 100 to 300. Dense bone above 300 HU.",
            &[
                (-190.0, Flat(BLACK)),
                (-30.0, Flat(YELLOW)),
                (20.0, Flat(BLUE)),
                (100.0, Flat(RED)),
                (300.0, Flat([224, 220, 210])),
                (TOP, Flat(WHITE)),
            ],
        ),
        banded(
            "CT Lung Density",
            (-1100.0, 0.0),
            "Below -950, -950 to -856, -856 to -500, -500 to -100, and above -100 HU. \
             Thresholds from human CT, not validated in animals.",
            &[
                (-950.0, Flat(RED)),
                (-856.0, Flat(AMBER)),
                (-500.0, Flat([110, 110, 110])),
                (-100.0, Flat([190, 190, 190])),
                (TOP, Flat(WHITE)),
            ],
        ),
        banded(
            "CT Calcium",
            (-50.0, 550.0),
            "Gray below 130. Then 130 to 199, 200 to 299, 300 to 399, and 400 HU and above. \
             Thresholds from human CT, not validated in animals.",
            &[
                (130.0, Ramp(BLACK, GREY)),
                (200.0, Flat(YELLOW)),
                (300.0, Flat(AMBER)),
                (400.0, Flat(RED)),
                (TOP, Flat(MAGENTA)),
            ],
        ),
        banded(
            "CT Body Composition",
            (-250.0, 250.0),
            "Fat -190 to -30. Muscle -29 to 150. Above that, bone and contrast HU.",
            &[
                (-190.0, Flat(DARK)),
                (-29.5, Flat(YELLOW)),
                (150.0, Flat(RED)),
                (TOP, Flat(WHITE)),
            ],
        ),
        banded(
            "CT Bone Density",
            (-100.0, 300.0),
            "Gray below 0. Then 0 to 100, 100 to 160, and 160 HU and above. \
             Thresholds from human CT, not validated in animals.",
            &[
                (0.0, Ramp(BLACK, GREY)),
                (100.0, Flat(RED)),
                (160.0, Flat(AMBER)),
                (TOP, Flat(GREEN)),
            ],
        ),
    ]
}

impl Clut {
    /// The identity grayscale table Horos calls "No CLUT".
    pub fn grayscale() -> Self {
        Self {
            name: "No CLUT".to_string(),
            table: (0..=255u8).map(|v| [v, v, v]).collect(),
            group: ClutGroup::Gray,
            span: None,
            about: "",
        }
    }

    /// The window, as centre and width, that puts every colour on its value.
    pub fn window(&self) -> Option<(f32, f32)> {
        // As the shader reads a window: its first and last values are
        // centre - width / 2 and centre + width / 2 - 1.
        self.span
            .map(|(low, high)| ((low + high + 1.0) / 2.0, high - low + 1.0))
    }

    /// Whether a window leaves the colours standing for what they say.
    pub fn shows_true_values(&self, center: f32, width: f32) -> bool {
        self.window()
            .is_none_or(|(c, w)| (c - center).abs() < 0.5 && (w - width).abs() < 0.5)
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
            group: ClutGroup::of(name),
            span: None,
            about: "",
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
        cluts.extend(ct_maps());
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

    /// The tables that make sense at any window, for a default.
    pub fn any_window_names(&self) -> impl Iterator<Item = &str> {
        self.cluts.iter().filter(|c| c.span.is_none()).map(|c| c.name.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_embedded_cluts_parse() {
        let lib = ClutLibrary::load();
        assert_eq!(lib.cluts.len(), EMBEDDED.len() + 1 + ct_maps().len());
        let hot_iron = lib.get("HotIron").unwrap();
        assert_eq!(hot_iron.table.len(), 256);
        assert_eq!(hot_iron.table[0], [0, 0, 0]);
        assert_eq!(hot_iron.table[255], [255, 255, 255]);
    }

    /// What the shader does with a value, at a map's own window.
    fn colour_at(clut: &Clut, value: f32) -> [u8; 3] {
        let (center, width) = clut.window().unwrap();
        let t = ((value - (center - 0.5)) / (width - 1.0) + 0.5).clamp(0.0, 1.0);
        clut.table[(t * 255.0).round() as usize]
    }

    #[test]
    fn a_map_window_runs_exactly_over_its_span() {
        for clut in ct_maps() {
            let (low, high) = clut.span.unwrap();
            let (center, width) = clut.window().unwrap();
            let first = (low - (center - 0.5)) / (width - 1.0) + 0.5;
            let last = (high - (center - 0.5)) / (width - 1.0) + 0.5;
            assert!(first.abs() < 1e-4 && (last - 1.0).abs() < 1e-4, "{}", clut.name);
            assert!(clut.shows_true_values(center, width));
            assert!(!clut.shows_true_values(center + 5.0, width));
        }
    }

    #[test]
    fn tissues_land_in_their_own_bands() {
        let lib = ClutLibrary::load();
        let t = lib.get("CT Tissues").unwrap();
        assert_eq!(colour_at(t, -1000.0), BLACK, "air, clamped");
        assert_eq!(colour_at(t, -100.0), YELLOW, "fat");
        assert_eq!(colour_at(t, 0.0), BLUE, "fluid");
        assert_eq!(colour_at(t, 50.0), RED, "soft tissue");
        assert_eq!(colour_at(t, 1500.0), WHITE, "dense bone");
        let c = lib.get("CT Calcium").unwrap();
        assert_eq!(colour_at(c, 150.0), YELLOW);
        assert_eq!(colour_at(c, 450.0), MAGENTA);
        let b = lib.get("CT Bone Density").unwrap();
        assert_eq!(colour_at(b, 50.0), RED);
        assert_eq!(colour_at(b, 200.0), GREEN);
        let l = lib.get("CT Lung Density").unwrap();
        assert_eq!(colour_at(l, -1000.0), RED);
        assert_eq!(colour_at(l, -900.0), AMBER);
    }

    #[test]
    fn a_band_edge_falls_within_a_table_step() {
        // One entry of CT Tissues is under 4 HU.
        let t = ct_maps().remove(0);
        let (low, high) = t.span.unwrap();
        assert!((high - low) / 255.0 < 4.0);
    }

    #[test]
    fn every_table_has_a_home_and_a_default_can_only_be_any_window() {
        let lib = ClutLibrary::load();
        assert_eq!(lib.cluts.iter().filter(|c| c.group == ClutGroup::Ct).count(), 5);
        assert_eq!(lib.get("PPU Viridis").unwrap().group, ClutGroup::Scientific);
        assert_eq!(lib.get("VR Bones").unwrap().group, ClutGroup::Rendering);
        assert_eq!(lib.get("HotIron").unwrap().group, ClutGroup::Classic);
        assert!(!lib.any_window_names().any(|n| n.starts_with("CT ")));
    }

    #[test]
    fn grayscale_is_identity() {
        let g = Clut::grayscale();
        assert_eq!(g.table[128], [128, 128, 128]);
        assert_eq!(g.to_rgba_bytes().len(), 1024);
    }
}
