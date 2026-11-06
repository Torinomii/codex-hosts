//! Shared palette and controls for explicit execution authorization.
use eframe::egui::{self, Color32, RichText};
use std::time::{Duration, Instant};

use crate::i18n::Catalog;

pub(crate) const BLUE: Color32 = Color32::from_rgb(25, 73, 137);
pub(crate) const RED: Color32 = Color32::from_rgb(190, 38, 51);
pub(crate) const INK: Color32 = Color32::from_rgb(28, 43, 64);
/// Warnings and the sudo badge, which carries INK text.
pub(crate) const WARN: Color32 = Color32::from_rgb(255, 226, 157);

pub(crate) fn frame(context: &egui::Context) -> egui::Frame {
    egui::Frame::window(&context.global_style())
        .fill(BLUE)
        .stroke(egui::Stroke::new(1.0, Color32::from_rgb(98, 154, 221)))
        .inner_margin(egui::Margin::same(18))
        .corner_radius(10.0)
}

pub(crate) fn style(ui: &mut egui::Ui) {
    let visuals = ui.visuals_mut();
    visuals.override_text_color = Some(Color32::WHITE);
    visuals.weak_text_color = Some(Color32::from_rgb(214, 230, 250));
    visuals.widgets.noninteractive.fg_stroke.color = Color32::from_rgb(214, 230, 250);
    visuals.widgets.noninteractive.bg_stroke.color = Color32::from_rgb(92, 139, 195);
    visuals.extreme_bg_color = Color32::from_rgb(18, 56, 109);
    visuals.warn_fg_color = WARN;
    visuals.selection.bg_fill = Color32::from_rgb(67, 118, 187);
    visuals.selection.stroke.color = Color32::WHITE;
}

pub(crate) struct Buttons {
    pub(crate) decision: Option<bool>,
    #[cfg(test)]
    pub(crate) confirm: egui::Rect,
    #[cfg(test)]
    pub(crate) cancel: egui::Rect,
}

pub(crate) fn buttons(ui: &mut egui::Ui, catalog: &Catalog, enabled: bool) -> Buttons {
    actions(
        ui,
        enabled,
        catalog.text("temp_approve"),
        catalog.text("temp_reject"),
    )
}

/// The allow (red, it starts something) and reject buttons, right-aligned;
/// the texts are already translated.
pub(crate) fn actions(ui: &mut egui::Ui, enabled: bool, allow: &str, reject: &str) -> Buttons {
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), 36.0),
        egui::Layout::right_to_left(egui::Align::Center),
        |ui| {
            let confirm = ui.add_enabled(
                enabled,
                egui::Button::new(RichText::new(allow).color(Color32::WHITE).strong())
                    .fill(RED)
                    .stroke(egui::Stroke::new(1.0, Color32::from_rgb(227, 112, 121)))
                    .min_size(egui::vec2(136.0, 36.0)),
            );
            let cancel = ui.add(
                egui::Button::new(RichText::new(reject).color(INK))
                    .fill(Color32::WHITE)
                    .stroke(egui::Stroke::new(1.0, Color32::from_rgb(209, 221, 235)))
                    .min_size(egui::vec2(136.0, 36.0)),
            );
            Buttons {
                decision: if cancel.clicked() {
                    Some(false)
                } else if confirm.clicked() {
                    Some(true)
                } else {
                    None
                },
                #[cfg(test)]
                confirm: confirm.rect,
                #[cfg(test)]
                cancel: cancel.rect,
            }
        },
    )
    .inner
}

pub(crate) fn preview(text: &str) -> String {
    let mut chars = text.chars();
    let mut value: String = chars
        .by_ref()
        .take(140)
        .map(|c| match c {
            '\n' | '\r' => '↵',
            '\t' => ' ',
            _ => c,
        })
        .collect();
    if chars.next().is_some() {
        value.push('…');
    }
    value
}

pub(crate) fn countdown(catalog: &Catalog, expires: Instant) -> String {
    let seconds = expires
        .saturating_duration_since(Instant::now())
        .as_millis()
        .div_ceil(1000);
    catalog.format(
        "approval_expires_in",
        &[("time", &format!("{}:{:02}", seconds / 60, seconds % 60))],
    )
}

/// Seconds a newly shown request ignores clicks, so a click aimed at the
/// previous view never decides it.
pub(crate) const ARM_SECONDS: f64 = 0.25;
/// Longer pause when the next calls replace a decided batch on their own: a
/// double click on a bulk button must not reach the calls that follow.
pub(crate) const NEXT_BATCH_ARM_SECONDS: f64 = 0.6;
/// The overview lists this many commands of a call; the rest are in details.
pub(crate) const PREVIEW_COMMANDS: usize = 5;

/// Title and a plain sentence saying who asks for what.
pub(crate) fn header(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    ui.heading(title);
    ui.add(egui::Label::new(subtitle).wrap());
    ui.add_space(4.0);
}

/// The remaining time, turning to the warning color in the last minute.
pub(crate) fn countdown_label(ui: &mut egui::Ui, catalog: &Catalog, expires: Instant) {
    let urgent = expires.saturating_duration_since(Instant::now()) < Duration::from_secs(60);
    let color = if urgent {
        ui.visuals().warn_fg_color
    } else {
        ui.visuals().weak_text_color()
    };
    ui.label(RichText::new(countdown(catalog, expires)).color(color));
}

/// The heading of one part of a request.
pub(crate) fn section(ui: &mut egui::Ui, text: &str) {
    ui.add_space(4.0);
    ui.label(RichText::new(text).color(ui.visuals().weak_text_color()));
}

/// One line of exact program or command text on a darker panel; a click
/// opens the full details.
pub(crate) fn code_line(ui: &mut egui::Ui, text: &str, hover: &str) -> egui::Response {
    let fill = ui.visuals().extreme_bg_color;
    egui::Frame::new()
        .fill(fill)
        .corner_radius(6.0)
        .inner_margin(egui::Margin::symmetric(10, 6))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(
                egui::Label::new(RichText::new(preview(text)).monospace())
                    .wrap()
                    .selectable(false),
            );
        })
        .response
        .interact(egui::Sense::click())
        .on_hover_cursor(egui::CursorIcon::PointingHand)
        .on_hover_text(hover)
}

/// A highlighted line for what raises the risk of a request, such as sudo.
/// A filled badge rather than a symbol, which the bundled fonts may lack.
pub(crate) fn warning(ui: &mut egui::Ui, text: &str) {
    let fill = ui.visuals().warn_fg_color;
    egui::Frame::new()
        .fill(fill)
        .corner_radius(4.0)
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.add(egui::Label::new(RichText::new(text).color(INK).strong()).wrap());
        });
}

/// Which key does what in this view.
pub(crate) fn key_hint(ui: &mut egui::Ui, text: &str) {
    ui.add(egui::Label::new(RichText::new(text).color(ui.visuals().weak_text_color())).wrap());
}

/// Runs `show` with disabled content drawn at full opacity. A window behind
/// an approval stays solid under the modal's backdrop, which already blocks
/// it, instead of turning see-through over the main window.
pub(crate) fn opaque_when_disabled<R>(context: &egui::Context, show: impl FnOnce() -> R) -> R {
    let style = context.global_style();
    context.global_style_mut(|style| style.visuals.disabled_alpha = 1.0);
    let result = show();
    context.set_global_style(style);
    result
}

pub(crate) fn size(bytes: usize) -> String {
    match bytes {
        0..1024 => format!("{bytes} B"),
        1024..1_048_576 => format!("{:.1} KiB", bytes as f64 / 1024.0),
        _ => format!("{:.1} MiB", bytes as f64 / 1_048_576.0),
    }
}

/// Consume Escape even during transitions, but do not turn key repeat into a
/// decision on a newly displayed request.
pub(crate) fn escape(context: &egui::Context) -> bool {
    context.input_mut(|input| {
        let pressed = input.events.iter().any(|event| matches!(event,
            egui::Event::Key { key: egui::Key::Escape, pressed: true, repeat: false, modifiers, .. }
                if *modifiers == egui::Modifiers::NONE
        ));
        input.consume_key(egui::Modifiers::NONE, egui::Key::Escape);
        pressed
    })
}

/// Bound layout work for large stdin/commands. All pages and full-text copying
/// retain the exact source; only the overview preview is abbreviated.
pub(crate) fn full_text(ui: &mut egui::Ui, catalog: &Catalog, id: egui::Id, text: &str) {
    const PAGE_BYTES: usize = 8192;
    let pages = text.len().div_ceil(PAGE_BYTES).max(1);
    let mut page = ui
        .ctx()
        .data(|data| data.get_temp::<usize>(id))
        .unwrap_or(0)
        .min(pages - 1);
    ui.horizontal_wrapped(|ui| {
        let copy = ui.small_button(catalog.text("approval_copy_full"));
        if copy.clicked() {
            ui.ctx().copy_text(text.to_owned());
        }
        #[cfg(test)]
        ui.ctx()
            .data_mut(|data| data.insert_temp(egui::Id::new(("approval_copy", id)), copy.rect));
        if pages > 1 {
            if ui
                .add_enabled(
                    page > 0,
                    egui::Button::new(catalog.text("approval_previous_page")),
                )
                .clicked()
            {
                page -= 1;
            }
            ui.label(catalog.format(
                "approval_text_page",
                &[
                    ("page", &(page + 1).to_string()),
                    ("pages", &pages.to_string()),
                ],
            ));
            let next = ui.add_enabled(
                page + 1 < pages,
                egui::Button::new(catalog.text("approval_next_page")),
            );
            if next.clicked() {
                page += 1;
            }
            #[cfg(test)]
            ui.ctx().data_mut(|data| {
                data.insert_temp(egui::Id::new(("approval_next_page", id)), next.rect)
            });
        }
    });
    ui.ctx().data_mut(|data| data.insert_temp(id, page));
    let boundary = |mut index: usize| {
        index = index.min(text.len());
        while !text.is_char_boundary(index) {
            index -= 1;
        }
        index
    };
    let start = boundary(page * PAGE_BYTES);
    let end = boundary((page + 1) * PAGE_BYTES);
    ui.add(
        egui::Label::new(RichText::new(&text[start..end]).monospace())
            .selectable(true)
            .wrap(),
    );
}

/// How a result reads: plain, done, needs attention, or went wrong.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum Tone {
    #[default]
    Info,
    Success,
    Warning,
    Error,
}

impl Tone {
    /// The tone's color in the application theme (not the approval palette).
    pub(crate) fn color(self, visuals: &egui::Visuals) -> Color32 {
        match self {
            Tone::Info => visuals.selection.bg_fill,
            Tone::Success => crate::ui::theme::success_color(visuals),
            Tone::Warning => visuals.warn_fg_color,
            Tone::Error => visuals.error_fg_color,
        }
    }
}

/// Seconds a result stays in view.
const FEEDBACK_SECONDS: u64 = 7;

#[derive(Default)]
pub(crate) struct Feedback {
    message: Option<(String, Tone, Instant)>,
}
impl Feedback {
    pub(crate) fn set(&mut self, text: String) {
        self.set_tone(text, Tone::Info);
    }
    pub(crate) fn set_tone(&mut self, text: String, tone: Tone) {
        self.message = Some((text, tone, Instant::now()));
    }
    fn current(&self) -> Option<&(String, Tone, Instant)> {
        self.message
            .as_ref()
            .filter(|(_, _, at)| at.elapsed() < Duration::from_secs(FEEDBACK_SECONDS))
    }
    pub(crate) fn text(&self) -> Option<&str> {
        self.current().map(|(text, _, _)| text.as_str())
    }
    pub(crate) fn tone(&self) -> Option<Tone> {
        self.current().map(|(_, tone, _)| *tone)
    }
    /// A toast in the application's own style, for when no window of ours
    /// can carry the message (the editor is hidden).
    pub(crate) fn show(&self, context: &egui::Context, id: &str) {
        let Some((text, tone, _)) = self.current() else {
            return;
        };
        context.request_repaint_after(Duration::from_millis(250));
        egui::Area::new(egui::Id::new(id))
            .anchor(egui::Align2::RIGHT_BOTTOM, [-20.0, -48.0])
            .order(egui::Order::Foreground)
            .interactable(false)
            .fade_in(false)
            .show(context, |ui| {
                let color = tone.color(ui.visuals());
                egui::Frame::popup(ui.style())
                    .stroke(egui::Stroke::new(1.5, color))
                    .inner_margin(egui::Margin::symmetric(14, 10))
                    .show(ui, |ui| {
                        ui.set_max_width((context.content_rect().width() - 80.0).min(380.0));
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("●").color(color));
                            ui.add(egui::Label::new(text.as_str()).wrap());
                        });
                    });
            });
    }
}

#[cfg(test)]
pub(crate) fn assert_painted_palette(
    output: &egui::FullOutput,
    confirm: egui::Rect,
    cancel: egui::Rect,
) {
    fn contains_fill(shape: &egui::Shape, color: Color32, at: Option<egui::Pos2>) -> bool {
        match shape {
            egui::Shape::Rect(shape) => {
                shape.fill == color && at.is_none_or(|point| shape.rect.contains(point))
            }
            egui::Shape::Vec(shapes) => shapes.iter().any(|shape| contains_fill(shape, color, at)),
            _ => false,
        }
    }
    for (color, point) in [
        (BLUE, None),
        (RED, Some(confirm.center())),
        (Color32::WHITE, Some(cancel.center())),
    ] {
        assert!(
            output
                .shapes
                .iter()
                .any(|shape| contains_fill(&shape.shape, color, point)),
            "approval palette color {color:?} was not painted at {point:?}"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_text_pages_preserve_unicode_and_explicit_copy_returns_the_entire_source() {
        let context = egui::Context::default();
        let catalog = Catalog::for_locale(Some("en"));
        let id = egui::Id::new("unicode-approval-text");
        let source = format!("{}END-OF-COMMAND", "多行 α\n".repeat(2000));
        let frame = |events| {
            context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(820.0, 620.0),
                    )),
                    time: Some(context.input(|i| i.time) + 0.1),
                    events,
                    ..Default::default()
                },
                |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(480.0)
                        .show(ui, |ui| full_text(ui, &catalog, id, &source));
                },
            )
        };
        let click = |at: egui::Pos2| {
            let mut last = None;
            for pressed in [true, false] {
                last = Some(frame(vec![
                    egui::Event::PointerMoved(at),
                    egui::Event::PointerButton {
                        pos: at,
                        button: egui::PointerButton::Primary,
                        pressed,
                        modifiers: egui::Modifiers::NONE,
                    },
                ]));
            }
            last.unwrap()
        };
        fn content(shape: &egui::Shape, values: &mut String) {
            match shape {
                egui::Shape::Text(text)
                    if text.galley.job.text.contains("多行")
                        || text.galley.job.text.contains("END-OF-COMMAND") =>
                {
                    values.push_str(&text.galley.job.text)
                }
                egui::Shape::Vec(shapes) => {
                    for shape in shapes {
                        content(shape, values);
                    }
                }
                _ => {}
            }
        }
        let pages = source.len().div_ceil(8192);
        let mut reconstructed = String::new();
        for page in 0..pages {
            // Read the actual painted label, including a boundary crossing a multibyte character.
            let output = frame(vec![]);
            for shape in &output.shapes {
                content(&shape.shape, &mut reconstructed);
            }
            let copy = context
                .data(|data| data.get_temp::<egui::Rect>(egui::Id::new(("approval_copy", id))))
                .unwrap();
            let output = click(copy.center());
            assert!(output.platform_output.commands.iter().any(
                |command| matches!(command, egui::OutputCommand::CopyText(text) if text == &source)
            ));
            if page + 1 < pages {
                let next = context
                    .data(|data| {
                        data.get_temp::<egui::Rect>(egui::Id::new(("approval_next_page", id)))
                    })
                    .unwrap();
                click(next.center());
                assert_eq!(
                    context.data(|data| data.get_temp::<usize>(id)),
                    Some(page + 1)
                );
            }
        }
        assert_eq!(reconstructed, source);
    }

    #[test]
    fn authorization_palette_has_readable_text_contrast() {
        fn luminance(color: Color32) -> f32 {
            let channel = |v: u8| {
                let v = f32::from(v) / 255.0;
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(color.r()) + 0.7152 * channel(color.g()) + 0.0722 * channel(color.b())
        }
        for color in [BLUE, RED, INK] {
            assert!((1.05 / (luminance(color) + 0.05)) >= 4.5);
        }
        // Warning text on the blue frame, and the sudo badge's ink on it.
        let contrast = |a: Color32, b: Color32| {
            let (a, b) = (luminance(a) + 0.05, luminance(b) + 0.05);
            a.max(b) / a.min(b)
        };
        assert!(contrast(WARN, BLUE) >= 4.5);
        assert!(contrast(WARN, INK) >= 4.5);
    }
}
