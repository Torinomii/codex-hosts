use std::fs;
use std::path::Path;

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily};

pub(super) fn apply_style(context: &egui::Context) {
    context.all_styles_mut(|style| {
        style.spacing.button_padding = egui::vec2(12.0, 6.0);
        style.spacing.item_spacing = egui::vec2(8.0, 6.0);
        style.spacing.interact_size.y = 28.0;
        style.spacing.text_edit_width = 280.0;
        for widget in [
            &mut style.visuals.widgets.noninteractive,
            &mut style.visuals.widgets.inactive,
            &mut style.visuals.widgets.hovered,
            &mut style.visuals.widgets.active,
            &mut style.visuals.widgets.open,
        ] {
            widget.corner_radius = 6.into();
        }
        // Light-mode inputs are white on white cards; a hairline keeps them visible.
        if !style.visuals.dark_mode {
            style.visuals.widgets.inactive.bg_stroke =
                egui::Stroke::new(1.0, Color32::from_gray(205));
        }
    });
}

/// Height of every single-line control (text inputs, combos, buttons), so a label centred
/// on this height lines up with whatever control sits beside it.
pub(super) fn control_height(ui: &egui::Ui) -> f32 {
    ui.spacing().interact_size.y
}

/// Text-input padding that makes a single-line box exactly `control_height` tall and puts
/// the first line of a multiline box on the same baseline. (`TextEdit::min_size` only
/// constrains width in egui 0.35, so the height has to come from the margin.)
pub(super) fn input_margin(ui: &egui::Ui) -> egui::Margin {
    let row = ui.text_style_height(&egui::TextStyle::Body);
    let pad = ((control_height(ui) - row) / 2.0).round().max(2.0) as i8;
    egui::Margin::symmetric(4, pad)
}

pub(super) fn success_color(visuals: &egui::Visuals) -> Color32 {
    if visuals.dark_mode {
        Color32::from_rgb(96, 200, 132)
    } else {
        Color32::from_rgb(22, 128, 66)
    }
}

pub(super) fn section_fill(visuals: &egui::Visuals) -> Color32 {
    if visuals.dark_mode {
        Color32::from_gray(36)
    } else {
        Color32::WHITE
    }
}

pub(super) fn section_stroke(visuals: &egui::Visuals) -> egui::Stroke {
    visuals.widgets.noninteractive.bg_stroke
}

pub(super) fn accent_bar_fill(visuals: &egui::Visuals) -> Color32 {
    visuals
        .selection
        .bg_fill
        .linear_multiply(if visuals.dark_mode { 0.45 } else { 0.25 })
}

pub(super) fn font_candidates(locale: &str) -> Vec<(&'static str, &'static str)> {
    const SEGOE: (&str, &str) = ("segoe", r"C:\Windows\Fonts\segoeui.ttf");
    const YAHEI: (&str, &str) = ("yahei", r"C:\Windows\Fonts\msyh.ttc");
    const JHENGHEI: (&str, &str) = ("jhenghei", r"C:\Windows\Fonts\msjh.ttc");
    const MEIRYO: (&str, &str) = ("meiryo", r"C:\Windows\Fonts\meiryo.ttc");
    let regional = match locale {
        "zh-CN" => [YAHEI, JHENGHEI, MEIRYO],
        "zh-TW" => [JHENGHEI, YAHEI, MEIRYO],
        "ja" => [MEIRYO, YAHEI, JHENGHEI],
        _ => [YAHEI, JHENGHEI, MEIRYO],
    };
    std::iter::once(SEGOE).chain(regional).collect()
}

pub(crate) fn configure_fonts(context: &egui::Context, locale: &str) {
    let mut fonts = FontDefinitions::default();
    let mut installed = Vec::new();
    for (name, path) in font_candidates(locale) {
        if let Ok(bytes) = fs::read(Path::new(path)) {
            fonts
                .font_data
                .insert(name.to_owned(), FontData::from_owned(bytes).into());
            installed.push(name.to_owned());
        }
    }
    for family in [FontFamily::Proportional, FontFamily::Monospace] {
        if let Some(fonts_for_family) = fonts.families.get_mut(&family) {
            for name in installed.iter().rev() {
                fonts_for_family.insert(0, name.clone());
            }
        }
    }
    context.set_fonts(fonts);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn font_loading_prioritizes_the_active_language_and_keeps_cjk_fallbacks() {
        assert_eq!(font_candidates("en").len(), 4);
        assert_eq!(font_candidates("zh-CN").len(), 4);
        assert_eq!(font_candidates("zh-CN")[1].0, "yahei");
        assert_eq!(font_candidates("zh-TW")[1].0, "jhenghei");
        assert_eq!(font_candidates("ja")[1].0, "meiryo");
        assert_eq!(font_candidates("unknown"), font_candidates("en"));
    }

    #[test]
    fn semantic_colors_stay_readable_in_both_themes() {
        for visuals in [egui::Visuals::dark(), egui::Visuals::light()] {
            let background = visuals.panel_fill;
            let contrast = |color: Color32| {
                fn luminance(color: Color32) -> f32 {
                    let channel = |value: u8| {
                        let value = value as f32 / 255.0;
                        if value <= 0.03928 {
                            value / 12.92
                        } else {
                            ((value + 0.055) / 1.055).powf(2.4)
                        }
                    };
                    0.2126 * channel(color.r())
                        + 0.7152 * channel(color.g())
                        + 0.0722 * channel(color.b())
                }
                let (a, b) = (luminance(color) + 0.05, luminance(background) + 0.05);
                a.max(b) / a.min(b)
            };
            assert!(contrast(success_color(&visuals)) >= 3.0);
            assert!(contrast(section_fill(&visuals)) < 1.6);
        }
    }
}
