//! A colour chosen as RGB, HSL or a hex code.

use egui::{Color32, Id, Ui};

/// Hue in degrees, saturation and lightness from 0 to 1.
pub fn to_hsl([r, g, b]: [u8; 3]) -> [f32; 3] {
    let [r, g, b] = [r, g, b].map(|c| c as f32 / 255.0);
    let (high, low) = (r.max(g).max(b), r.min(g).min(b));
    let (range, lightness) = (high - low, (high + low) / 2.0);
    if range <= 0.0 {
        return [0.0, 0.0, lightness];
    }
    let saturation = range / (1.0 - (2.0 * lightness - 1.0).abs());
    let sector = if high == r {
        ((g - b) / range).rem_euclid(6.0)
    } else if high == g {
        (b - r) / range + 2.0
    } else {
        (r - g) / range + 4.0
    };
    [sector * 60.0, saturation.min(1.0), lightness]
}

pub fn from_hsl([hue, saturation, lightness]: [f32; 3]) -> [u8; 3] {
    let chroma = (1.0 - (2.0 * lightness - 1.0).abs()) * saturation;
    let sector = hue.rem_euclid(360.0) / 60.0;
    let middle = chroma * (1.0 - (sector.rem_euclid(2.0) - 1.0).abs());
    let (r, g, b) = match sector as u32 {
        0 => (chroma, middle, 0.0),
        1 => (middle, chroma, 0.0),
        2 => (0.0, chroma, middle),
        3 => (0.0, middle, chroma),
        4 => (middle, 0.0, chroma),
        _ => (chroma, 0.0, middle),
    };
    let base = lightness - chroma / 2.0;
    [r, g, b].map(|c| ((c + base) * 255.0).round().clamp(0.0, 255.0) as u8)
}

pub fn to_hex([r, g, b]: [u8; 3]) -> String {
    format!("#{r:02X}{g:02X}{b:02X}")
}

/// `#RRGGBB` or `#RGB`, with or without the `#`.
pub fn from_hex(text: &str) -> Option<[u8; 3]> {
    let digits = text.trim().trim_start_matches('#');
    if !digits.is_ascii() {
        return None;
    }
    let number = |s: &str| u8::from_str_radix(s, 16).ok();
    match digits.len() {
        6 => Some([number(&digits[..2])?, number(&digits[2..4])?, number(&digits[4..6])?]),
        3 => {
            let one = |i: usize| number(&digits[i..i + 1]).map(|n| n * 17);
            Some([one(0)?, one(1)?, one(2)?])
        }
        _ => None,
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Form {
    Rgb,
    Hsl,
    Hex,
}

/// What the editor holds between frames. Hue is kept here because grey has none:
/// taking it from the colour would send the slider to zero.
#[derive(Clone)]
struct Editing {
    form: Form,
    hsl: [f32; 3],
    hex: String,
    /// The colour these were last in step with.
    colour: [u8; 3],
}

/// Edit a colour. True when it changed.
pub fn editor(ui: &mut Ui, id: Id, colour: &mut [u8; 3]) -> bool {
    let id = id.with("colour-editor");
    let mut e = ui.data_mut(|d| d.get_temp::<Editing>(id)).unwrap_or(Editing {
        form: Form::Rgb,
        hsl: to_hsl(*colour),
        hex: to_hex(*colour),
        colour: *colour,
    });
    if e.colour != *colour {
        e.hsl = to_hsl(*colour);
        e.hex = to_hex(*colour);
        e.colour = *colour;
    }
    let before = *colour;
    let (swatch, _) = ui.allocate_exact_size(egui::vec2(220.0, 28.0), egui::Sense::hover());
    ui.painter()
        .rect_filled(swatch, 3.0, Color32::from_rgb(colour[0], colour[1], colour[2]));
    ui.horizontal(|ui| {
        ui.selectable_value(&mut e.form, Form::Rgb, "RGB");
        ui.selectable_value(&mut e.form, Form::Hsl, "HSL");
        ui.selectable_value(&mut e.form, Form::Hex, "#");
    });
    match e.form {
        Form::Rgb => {
            for (name, channel) in ["R", "G", "B"].into_iter().zip(colour.iter_mut()) {
                ui.add(egui::Slider::new(channel, 0..=255).text(name));
            }
        }
        Form::Hsl => {
            let mut moved = ui
                .add(
                    egui::Slider::new(&mut e.hsl[0], 0.0..=360.0)
                        .text("H")
                        .suffix("°")
                        .fixed_decimals(0),
                )
                .changed();
            for (at, name) in [(1, "S"), (2, "L")] {
                let mut percent = e.hsl[at] * 100.0;
                if ui
                    .add(
                        egui::Slider::new(&mut percent, 0.0..=100.0)
                            .text(name)
                            .suffix("%")
                            .fixed_decimals(0),
                    )
                    .changed()
                {
                    e.hsl[at] = percent / 100.0;
                    moved = true;
                }
            }
            if moved {
                *colour = from_hsl(e.hsl);
                e.colour = *colour;
                e.hex = to_hex(*colour);
            }
        }
        Form::Hex => {
            let box_ = ui.add(
                egui::TextEdit::singleline(&mut e.hex)
                    .desired_width(100.0)
                    .hint_text("#RRGGBB"),
            );
            if box_.changed() {
                if let Some(typed) = from_hex(&e.hex) {
                    *colour = typed;
                    e.colour = typed;
                    e.hsl = to_hsl(typed);
                }
            }
            if from_hex(&e.hex).is_none() && !e.hex.trim().is_empty() {
                ui.label(egui::RichText::new("Six hex digits, such as #F0C878").small());
            }
        }
    }
    ui.data_mut(|d| d.insert_temp(id, e));
    *colour != before
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_colours_have_their_known_hsl() {
        assert_eq!(to_hsl([255, 0, 0]), [0.0, 1.0, 0.5]);
        assert_eq!(to_hsl([0, 255, 0]), [120.0, 1.0, 0.5]);
        assert_eq!(to_hsl([0, 0, 255]), [240.0, 1.0, 0.5]);
        assert_eq!(to_hsl([255, 255, 255]), [0.0, 0.0, 1.0]);
        assert_eq!(from_hsl([60.0, 1.0, 0.5]), [255, 255, 0]);
        assert_eq!(from_hsl([300.0, 1.0, 0.25]), [128, 0, 128]);
    }

    #[test]
    fn every_colour_survives_hsl_and_back() {
        for r in (0..=255).step_by(5) {
            for g in (0..=255).step_by(5) {
                for b in (0..=255).step_by(5) {
                    let c = [r as u8, g as u8, b as u8];
                    assert_eq!(from_hsl(to_hsl(c)), c);
                }
            }
        }
    }

    #[test]
    fn hue_at_the_end_of_the_slider_is_the_start_again() {
        assert_eq!(from_hsl([360.0, 1.0, 0.5]), [255, 0, 0]);
    }

    #[test]
    fn hex_reads_what_people_type_and_refuses_the_rest() {
        assert_eq!(from_hex("#F0C878"), Some([240, 200, 120]));
        assert_eq!(from_hex(" f0c878 "), Some([240, 200, 120]));
        assert_eq!(from_hex("#0af"), Some([0, 170, 255]));
        assert_eq!(to_hex([240, 200, 120]), "#F0C878");
        for bad in ["", "#", "#12", "#12345", "#GGGGGG", "é12345"] {
            assert_eq!(from_hex(bad), None, "{bad}");
        }
    }
}
