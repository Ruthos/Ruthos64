//! Window level / width presets.
//!
//! The CT values are the ones radiologists expect and Horos ships by default.
//! For radiographs the useful presets are the header default and the full
//! dynamic range, which are computed per image rather than listed here.

/// A named window.
#[derive(Debug, Clone, PartialEq)]
pub struct WindowPreset {
    pub name: String,
    pub center: f32,
    pub width: f32,
}

impl WindowPreset {
    pub fn new(name: &str, center: f32, width: f32) -> Self {
        Self {
            name: name.to_string(),
            center,
            width,
        }
    }
}

/// Preset library, grouped the way the Horos WL/WW menu groups them.
#[derive(Debug, Clone)]
pub struct WindowPresets {
    pub presets: Vec<WindowPreset>,
}

impl Default for WindowPresets {
    fn default() -> Self {
        Self::horos_defaults()
    }
}

impl WindowPresets {
    pub fn horos_defaults() -> Self {
        Self {
            presets: vec![
                WindowPreset::new("CT - Abdomen", 40.0, 350.0),
                WindowPreset::new("CT - Bone", 300.0, 1500.0),
                WindowPreset::new("CT - Brain", 40.0, 80.0),
                WindowPreset::new("CT - Liver", 60.0, 160.0),
                WindowPreset::new("CT - Lung", -600.0, 1500.0),
                WindowPreset::new("CT - Mediastinum", 50.0, 350.0),
                WindowPreset::new("CT - Soft Tissue", 40.0, 400.0),
                WindowPreset::new("CT - Spine", 50.0, 250.0),
                WindowPreset::new("CT - Stroke", 35.0, 35.0),
            ],
        }
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.presets.iter().map(|p| p.name.as_str())
    }
}
