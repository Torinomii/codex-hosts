use std::fs;
use std::path::Path;

use eframe::egui::{self, Color32, FontData, FontDefinitions, FontFamily};

use crate::i18n::Catalog;

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
        // A hairline outlines every idle control so buttons and inputs read as
        // controls on any card, and disabled ones fade far enough to look idle.
        style.visuals.widgets.inactive.bg_stroke = egui::Stroke::new(
            1.0,
            if style.visuals.dark_mode {
                Color32::from_gray(78)
            } else {
                Color32::from_gray(205)
            },
        );
        style.visuals.disabled_alpha = 0.38;
    });
}

/// The accent behind a view's main action; white text stays readable on it in
/// both themes.
pub(crate) fn accent_fill(visuals: &egui::Visuals) -> Color32 {
    if visuals.dark_mode {
        Color32::from_rgb(38, 104, 186)
    } else {
        Color32::from_rgb(26, 96, 196)
    }
}

/// The one button a view leads with (save, next, unlock, confirm).
pub(crate) fn primary_button(ui: &egui::Ui, text: &str) -> egui::Button<'static> {
    egui::Button::new(
        egui::RichText::new(text.to_owned())
            .strong()
            .color(Color32::WHITE),
    )
    .fill(accent_fill(ui.visuals()))
    .stroke(egui::Stroke::NONE)
}

/// A button that leads to a hard-to-undo action (log out, remove, delete):
/// an ordinary button with red text, so it is recognisable without
/// shouting. The action itself is confirmed with [`danger_confirm_button`].
pub(crate) fn danger_button(ui: &egui::Ui, text: &str) -> egui::Button<'static> {
    egui::Button::new(egui::RichText::new(text.to_owned()).color(ui.visuals().error_fg_color))
}

/// The fill behind the button that carries out a hard-to-undo action;
/// white text stays readable on it in both themes.
pub(crate) fn danger_fill(visuals: &egui::Visuals) -> Color32 {
    if visuals.dark_mode {
        Color32::from_rgb(176, 46, 52)
    } else {
        Color32::from_rgb(190, 32, 40)
    }
}

/// The confirming button of a hard-to-undo action, styled like
/// [`primary_button`] in red.
pub(crate) fn danger_confirm_button(ui: &egui::Ui, text: &str) -> egui::Button<'static> {
    egui::Button::new(
        egui::RichText::new(text.to_owned())
            .strong()
            .color(Color32::WHITE),
    )
    .fill(danger_fill(ui.visuals()))
    .stroke(egui::Stroke::NONE)
}

/// Drops the undo snapshots egui keeps for a password field. They hold the
/// unmasked text, are never zeroized, and undo is of no use on a masked
/// field. Call it right after adding the field, every frame.
pub(crate) fn forget_password_history(ui: &egui::Ui, response: &egui::Response) {
    if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), response.id) {
        state.clear_undoer();
        egui::TextEdit::store_state(ui.ctx(), response.id, state);
    }
}

/// Whether the password field that `key` names shows its text. A reveal
/// lasts only while the field stays on screen; when it comes back (a dialog
/// opened again, another host's form) it starts masked.
pub(crate) fn password_revealed(ui: &egui::Ui, key: egui::Id) -> bool {
    let now = ui.ctx().cumulative_frame_nr();
    ui.data(|data| data.get_temp::<(bool, u64)>(key.with("reveal")))
        .is_some_and(|(shown, seen)| shown && seen + 1 >= now)
}

/// The show/hide button placed right after a password field: one click
/// shows the text (typing goes on in plain text), the next masks it again.
/// The field reads [`password_revealed`] with the same `key`.
pub(crate) fn reveal_button(ui: &mut egui::Ui, catalog: &Catalog, key: egui::Id) -> egui::Response {
    let mut shown = password_revealed(ui, key);
    let hint = catalog.text(if shown {
        "password_hide"
    } else {
        "password_show"
    });
    let side = control_height(ui);
    let mut response = ui
        .add_sized([side, side], egui::Button::new("").selected(shown))
        .on_hover_text(hint);
    if response.clicked() {
        shown = !shown;
        response.mark_changed();
    }
    let enabled = ui.is_enabled();
    response
        .widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::Button, enabled, shown, hint));
    let frame = ui.ctx().cumulative_frame_nr();
    ui.data_mut(|data| data.insert_temp(key.with("reveal"), (shown, frame)));
    // An eye; struck through while the text shows, since a click hides it.
    let color = ui
        .style()
        .interact_selectable(&response, shown)
        .fg_stroke
        .color;
    let center = response.rect.center();
    let stroke = egui::Stroke::new(1.4, color);
    let painter = ui.painter();
    painter.add(egui::Shape::ellipse_stroke(
        center,
        egui::vec2(side * 0.3, side * 0.18),
        stroke,
    ));
    painter.circle_filled(center, side * 0.09, color);
    if shown {
        let reach = egui::vec2(side * 0.28, side * 0.28);
        painter.line_segment([center - reach, center + reach], stroke);
    }
    response
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

pub(crate) fn section_fill(visuals: &egui::Visuals) -> Color32 {
    if visuals.dark_mode {
        Color32::from_gray(36)
    } else {
        Color32::WHITE
    }
}

pub(crate) fn section_stroke(visuals: &egui::Visuals) -> egui::Stroke {
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

    fn contrast(a: Color32, b: Color32) -> f32 {
        fn luminance(color: Color32) -> f32 {
            let channel = |value: u8| {
                let value = value as f32 / 255.0;
                if value <= 0.03928 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
        }
        let (a, b) = (luminance(a) + 0.05, luminance(b) + 0.05);
        a.max(b) / a.min(b)
    }

    #[test]
    fn password_fields_keep_no_undo_snapshots_of_what_was_typed() {
        for forget in [false, true] {
            let context = egui::Context::default();
            let id = egui::Id::new("password_under_test");
            let mut text = String::new();
            context.memory_mut(|memory| memory.request_focus(id));
            for (index, typed) in ["", "secret", "", ""].into_iter().enumerate() {
                let _ = context.run_ui(
                    egui::RawInput {
                        // Far enough apart for egui to take an undo point.
                        time: Some(index as f64 * 2.0),
                        events: if typed.is_empty() {
                            vec![]
                        } else {
                            vec![egui::Event::Text(typed.into())]
                        },
                        ..Default::default()
                    },
                    |ui| {
                        let response =
                            ui.add(egui::TextEdit::singleline(&mut text).password(true).id(id));
                        if forget {
                            forget_password_history(ui, &response);
                        }
                    },
                );
            }
            assert_eq!(text, "secret", "forget = {forget}");
            let probe = (egui::text::CCursorRange::default(), String::from("\0"));
            let kept = egui::TextEdit::load_state(&context, id)
                .unwrap()
                .undoer()
                .has_undo(&probe);
            assert_eq!(kept, !forget, "forget = {forget}");
        }
    }

    #[test]
    fn the_eye_button_shows_the_password_while_typing_until_the_field_leaves() {
        let context = egui::Context::default();
        configure_fonts(&context, "en");
        let catalog = Catalog::for_locale(Some("en"));
        let key = egui::Id::new("reveal_under_test");
        let mut text = String::from("sec");
        let button = std::cell::Cell::new(egui::Rect::NOTHING);
        let run = |text: &mut String, show: bool, events: Vec<egui::Event>| {
            let output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(600.0, 200.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    if show {
                        ui.horizontal(|ui| {
                            let revealed = password_revealed(ui, key);
                            ui.add(egui::TextEdit::singleline(text).id(key).password(!revealed));
                            button.set(reveal_button(ui, &catalog, key).rect);
                        });
                    }
                },
            );
            crate::ui::tests::painted_texts(&output.shapes)
                .into_iter()
                .map(|(text, _, _)| text)
                .collect::<Vec<_>>()
        };
        let painted = run(&mut text, true, vec![]);
        assert!(!painted.iter().any(|t| t.contains("sec")), "{painted:?}");
        let at = button.get().center();
        for pressed in [true, false] {
            run(
                &mut text,
                true,
                vec![
                    egui::Event::PointerMoved(at),
                    egui::Event::PointerButton {
                        pos: at,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ],
            );
        }
        context.memory_mut(|memory| memory.request_focus(key));
        run(&mut text, true, vec![]);
        let painted = run(&mut text, true, vec![egui::Event::Text("ret".into())]);
        assert_eq!(text, "secret", "typing goes on while the text shows");
        let painted = [painted, run(&mut text, true, vec![])].concat();
        assert!(painted.iter().any(|t| t == "secret"), "{painted:?}");
        // A frame without the field (dialog closed) ends the reveal.
        run(&mut text, false, vec![]);
        let painted = run(&mut text, true, vec![]);
        assert!(!painted.iter().any(|t| t.contains("sec")), "{painted:?}");
    }

    #[test]
    fn semantic_colors_stay_readable_in_both_themes() {
        for visuals in [egui::Visuals::dark(), egui::Visuals::light()] {
            let background = visuals.panel_fill;
            assert!(contrast(success_color(&visuals), background) >= 3.0);
            assert!(
                contrast(accent_fill(&visuals), background) >= 1.8,
                "the accent stands out"
            );
            assert!(contrast(section_fill(&visuals), background) < 1.6);
            for fill in [accent_fill(&visuals), danger_fill(&visuals)] {
                assert!(
                    contrast(Color32::WHITE, fill) >= 4.5,
                    "white button text on {fill:?}"
                );
            }
        }
    }
}
