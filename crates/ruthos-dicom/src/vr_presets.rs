//! Horos's volume rendering presets, read the way Horos reads them.
//!
//! A preset is one of two kinds. An *advanced* one holds opacity curves over
//! the values, with a colour at each point. Horos merges the points of every
//! curve into one function, squares each opacity, and fades to clear at the
//! ends of the volume's range. A *classic* one names an 8-bit colour table and
//! an opacity table, spread over its window: clear below it, and opaque above.
//! See `VRView.mm` (`setAdvancedCLUT`, `setOpacity`) and `DefaultsOsiriX.m`.

use std::io::Cursor;

use crate::clut::ClutLibrary;

/// A preset as colour and opacity at values, ready for a transfer function.
#[derive(Debug, Clone, PartialEq)]
pub struct VrPreset {
    pub name: String,
    /// Horos's group: "Bone CT", "Soft Tissue CT" or "Basic".
    pub group: String,
    /// Value, then red, green, blue and opacity from 0 to 1, by value.
    pub stops: Vec<(f32, [f32; 4])>,
    /// Clear below the volume's lowest value and above its highest, with the
    /// opacity sloping to them from the outer points.
    pub ends_clear: bool,
}

macro_rules! embedded_presets {
    ($($name:literal),* $(,)?) => {
        &[
            $(($name, include_bytes!(concat!("../../../assets/horos/3dpresets/", $name, ".plist")) as &[u8]),)*
        ]
    };
}

/// (name, plist bytes) for every 3D preset shipped with Horos.
const EMBEDDED: &[(&str, &[u8])] = embedded_presets![
    "Airways II",
    "Airways",
    "Black & White",
    "Bone + Skin II",
    "Bone + Skin",
    "Dark Bone",
    "Glossy II",
    "Glossy",
    "Gold Bone",
    "High Contrast",
    "Low Contrast",
    "Mid Contrast",
    "No Shading",
    "Pencil",
    "Red on White",
    "Skin On Blue II",
    "Skin On Blue",
    "Soft + Skin II",
    "Soft + Skin III",
    "Soft + Skin",
    "Soft On Blue",
    "Soft on White",
    "Soft",
    "Standard",
    "Vascular II",
    "Vascular III",
    "Vascular IV",
    "Vascular",
    "Yellow Bone",
];

/// Points a classic preset's window is sampled at.
const TABLE_STEPS: usize = 48;

/// Every Horos preset, by group and then name. One that cannot be read is
/// left out with a warning.
pub fn horos_presets(cluts: &ClutLibrary) -> Vec<VrPreset> {
    let mut out: Vec<VrPreset> = EMBEDDED
        .iter()
        .filter_map(|(name, bytes)| {
            let preset = parse(bytes, cluts);
            if preset.is_none() {
                tracing::warn!("Horos 3D preset {name} could not be read");
            }
            preset
        })
        .collect();
    out.sort_by(|a, b| a.group.cmp(&b.group).then(a.name.cmp(&b.name)));
    out
}

fn number(value: &plist::Value) -> Option<f64> {
    value
        .as_real()
        .or_else(|| value.as_signed_integer().map(|i| i as f64))
        .or_else(|| value.as_unsigned_integer().map(|u| u as f64))
}

/// One preset from its plist.
pub fn parse(bytes: &[u8], cluts: &ClutLibrary) -> Option<VrPreset> {
    let value = plist::Value::from_reader(Cursor::new(bytes)).ok()?;
    let dict = value.as_dictionary()?;
    let text = |key: &str| {
        dict.get(key)
            .and_then(|v| v.as_string())
            .unwrap_or_default()
            .to_string()
    };
    let name = text("name");
    let group = text("groupName");
    let advanced = dict.get("advancedCLUT").and_then(|v| v.as_boolean()).unwrap_or(false);
    if advanced {
        let curves = dict.get("16bitClutCurves")?.as_array()?;
        let colours = dict.get("16bitClutColors")?.as_array()?;
        let mut stops = Vec::new();
        for (curve, colours) in curves.iter().zip(colours) {
            for (point, colour) in curve.as_array()?.iter().zip(colours.as_array()?) {
                let point = point.as_dictionary()?;
                let colour = colour.as_dictionary()?;
                let c = |key: &str| colour.get(key).and_then(number).unwrap_or(0.0) as f32;
                let y = point.get("y").and_then(number)? as f32;
                stops.push((
                    point.get("x").and_then(number)? as f32,
                    [c("red"), c("green"), c("blue"), (y * y).clamp(0.0, 1.0)],
                ));
            }
        }
        if stops.is_empty() {
            return None;
        }
        // Horos adds every point to one function, which keeps them in order.
        stops.sort_by(|a, b| a.0.total_cmp(&b.0));
        return Some(VrPreset {
            name,
            group,
            stops,
            ends_clear: true,
        });
    }
    let wl = dict.get("wl").and_then(number)? as f32;
    let ww = (dict.get("ww").and_then(number)? as f32).max(1.0);
    let table = match text("CLUT").as_str() {
        "No CLUT" | "" => None,
        clut => cluts.cluts.iter().find(|c| c.name == clut),
    };
    let opacity = opacity_table(&text("opacity"));
    let start = wl - ww / 2.0;
    let stops = (0..=TABLE_STEPS)
        .map(|i| {
            let t = i as f32 / TABLE_STEPS as f32;
            let [r, g, b] = match table {
                Some(c) => c.table[((t * 255.0).round() as usize).min(255)].map(|v| v as f32 / 255.0),
                None => [t; 3],
            };
            (start + t * ww, [r, g, b, opacity(t)])
        })
        .collect();
    Some(VrPreset {
        name,
        group,
        stops,
        ends_clear: false,
    })
}

/// Horos's named opacity tables, from clear at the bottom of the window to
/// opaque at the top.
fn opacity_table(name: &str) -> fn(f32) -> f32 {
    match name {
        "Logarithmic Table" => |t| (1.0 + 9.0 * t).log10(),
        "Logarithmic Inverse Table" => |t| 1.0 - (1.0 + 9.0 * (1.0 - t)).log10(),
        // One point, 0.05 at 180 of 256, between clear and opaque.
        "Smooth Table" => |t| {
            const KNEE: f32 = 180.0 / 256.0;
            if t <= KNEE {
                0.05 * t / KNEE
            } else {
                0.05 + 0.95 * (t - KNEE) / (1.0 - KNEE)
            }
        },
        _ => |t| t,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_horos_preset_reads() {
        let presets = horos_presets(&ClutLibrary::load());
        assert_eq!(presets.len(), EMBEDDED.len());
        let groups: std::collections::BTreeSet<&str> = presets.iter().map(|p| p.group.as_str()).collect();
        assert_eq!(
            groups.into_iter().collect::<Vec<_>>(),
            ["Basic", "Bone CT", "Soft Tissue CT"]
        );
        for p in &presets {
            assert!(p.stops.windows(2).all(|w| w[0].0 <= w[1].0), "{} in order", p.name);
            assert!(
                p.stops.iter().all(|s| s.1.iter().all(|c| (0.0..=1.0).contains(c))),
                "{}",
                p.name
            );
        }
    }

    #[test]
    fn an_advanced_preset_squares_its_curves() {
        let presets = horos_presets(&ClutLibrary::load());
        let p = presets.iter().find(|p| p.name == "Bone + Skin").unwrap();
        assert!(p.ends_clear);
        assert_eq!(p.stops.len(), 9, "five points and four");
        let first = p.stops[0];
        assert!((first.0 + 713.844).abs() < 0.01 && first.1[3] == 0.0);
        // y = 0.334 on the bone curve, so 0.1116 once squared.
        let last = p.stops.last().unwrap();
        assert!((last.0 - 333.10).abs() < 0.01);
        assert!((last.1[3] - 0.334f32.powi(2)).abs() < 1e-3, "{}", last.1[3]);
    }

    #[test]
    fn a_classic_preset_spans_its_window() {
        let presets = horos_presets(&ClutLibrary::load());
        let p = presets.iter().find(|p| p.name == "Mid Contrast").unwrap();
        assert!(!p.ends_clear);
        let (low, high) = (p.stops[0], *p.stops.last().unwrap());
        assert!((low.0 - (257.05 - 296.28 / 2.0)).abs() < 0.01);
        assert!((high.0 - (257.05 + 296.28 / 2.0)).abs() < 0.01);
        assert_eq!((low.1[3], high.1[3]), (0.0, 1.0), "linear: clear to opaque");
        let log = presets.iter().find(|p| p.name == "High Contrast").unwrap();
        let inverse = presets.iter().find(|p| p.name == "Low Contrast").unwrap();
        let middle = TABLE_STEPS / 2;
        assert!(log.stops[middle].1[3] > 0.7 && inverse.stops[middle].1[3] < 0.3);
        let pencil = presets.iter().find(|p| p.name == "Pencil").unwrap();
        let grey = pencil.stops[middle].1;
        assert!(grey[0] == grey[1] && grey[1] == grey[2], "no colour table is grey");
    }
}
