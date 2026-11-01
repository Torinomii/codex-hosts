use eframe::egui::{self, Color32, RichText};

use super::{HostTestState, HostsApp, retain_visible_selection};
use crate::i18n::Catalog;
use crate::model::{HostFilter, HostProfile, Protocol, normalize_tags};

// Dark-mode fills sit under light text; light-mode fills under dark text.
pub(super) const TEST_SUCCESS_FILL_DARK: Color32 = Color32::from_rgb(36, 105, 67);
pub(super) const TEST_FAILURE_FILL_DARK: Color32 = Color32::from_rgb(132, 48, 53);
pub(super) const TEST_SUCCESS_FILL_LIGHT: Color32 = Color32::from_rgb(198, 232, 206);
pub(super) const TEST_FAILURE_FILL_LIGHT: Color32 = Color32::from_rgb(246, 200, 203);

/// Horizontal inset shared by the list title, the search box text and the row text, so
/// every left edge in the panel lines up.
pub(super) const LIST_INSET: f32 = 12.0;
const ROW_PADDING_Y: f32 = 9.0;
const ROW_LINE_GAP: f32 = 3.0;
const ROW_ACCENT_WIDTH: f32 = 3.0;
const DETAIL_SIZE: f32 = 11.5;
const SECONDARY_SIZE: f32 = 11.0;
const BADGE_SIZE: f32 = 10.0;
const BADGE_HEIGHT: f32 = 14.0;
const BADGE_PADDING_X: f32 = 4.0;
/// Badges that would squeeze the alias below this width stay in the hover text only.
const MIN_ALIAS_WIDTH: f32 = 72.0;

pub(super) fn host_row_fill(state: Option<HostTestState>, dark_mode: bool) -> Option<Color32> {
    match (state, dark_mode) {
        (Some(HostTestState::Succeeded), true) => Some(TEST_SUCCESS_FILL_DARK),
        (Some(HostTestState::Succeeded), false) => Some(TEST_SUCCESS_FILL_LIGHT),
        (Some(HostTestState::Failed), true) => Some(TEST_FAILURE_FILL_DARK),
        (Some(HostTestState::Failed), false) => Some(TEST_FAILURE_FILL_LIGHT),
        _ => None,
    }
}

fn line_height(ui: &egui::Ui, size: f32) -> f32 {
    let font = egui::FontId::proportional(size);
    ui.fonts_mut(|fonts| fonts.row_height(&font))
}

/// Rows are two lines (alias + endpoint) plus an optional third line for tags or notes.
pub(super) fn host_row_height(ui: &egui::Ui, three_lines: bool) -> f32 {
    let alias = ui.text_style_height(&egui::TextStyle::Body);
    let detail = line_height(ui, DETAIL_SIZE);
    let mut height = ROW_PADDING_Y * 2.0 + alias + ROW_LINE_GAP + detail;
    if three_lines {
        height += ROW_LINE_GAP + line_height(ui, SECONDARY_SIZE);
    }
    height
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RowMarker {
    Verified,
    Testing,
}

/// Shown after the endpoint for hosts that keep their authenticated session
/// between Codex calls, so the relaxed hosts stand out in the list.
pub(super) const RETAINED_GLYPH: &str = "∞";

/// A feature enabled on the host, shown as a small pill after the alias.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum RowBadge {
    Sudo,
    Su,
    CallApproval,
    Jump,
    CodexHidden,
}

impl RowBadge {
    fn label(self, catalog: &Catalog) -> &str {
        match self {
            Self::Sudo => "sudo",
            Self::Su => "su",
            Self::CallApproval => catalog.text("badge_call_approval"),
            Self::Jump => catalog.text("badge_jump"),
            Self::CodexHidden => catalog.text("badge_codex_hidden"),
        }
    }

    fn hint(self, catalog: &Catalog) -> &str {
        catalog.text(match self {
            Self::Sudo => "sudo_enabled",
            Self::Su => "su_enabled",
            Self::CallApproval => "ssh_call_approval",
            Self::Jump => "host_chain",
            Self::CodexHidden => "codex_hidden",
        })
    }
}

fn row_badges(host: &HostProfile) -> Vec<RowBadge> {
    let advanced = &host.advanced;
    let ssh = host.protocol == Protocol::Ssh;
    [
        (
            ssh && advanced.sudo.as_ref().is_some_and(|sudo| sudo.enabled),
            RowBadge::Sudo,
        ),
        (
            ssh && advanced.su.as_ref().is_some_and(|su| su.enabled),
            RowBadge::Su,
        ),
        (
            ssh && advanced.require_call_approval,
            RowBadge::CallApproval,
        ),
        (ssh && host.jump_host.is_some(), RowBadge::Jump),
        (advanced.codex_hidden, RowBadge::CodexHidden),
    ]
    .into_iter()
    .filter_map(|(enabled, badge)| enabled.then_some(badge))
    .collect()
}

pub(super) struct HostRowModel<'a> {
    pub alias: &'a str,
    pub detail: String,
    pub marker: Option<RowMarker>,
    pub retained: bool,
    pub badges: Vec<RowBadge>,
    /// Tags when present, otherwise the first line of the description.
    pub secondary: String,
    pub test_state: Option<HostTestState>,
    pub selected: bool,
    pub checked: Option<bool>,
}

impl<'a> HostRowModel<'a> {
    pub fn from_profile(
        host: &'a HostProfile,
        test_state: Option<HostTestState>,
        selected: bool,
        checked: Option<bool>,
    ) -> Self {
        let marker = if test_state == Some(HostTestState::Succeeded)
            || (test_state.is_none() && host.verified)
        {
            Some(RowMarker::Verified)
        } else if test_state == Some(HostTestState::Testing) {
            Some(RowMarker::Testing)
        } else {
            None
        };
        let secondary = if host.tags.is_empty() {
            host.description.lines().next().unwrap_or("").to_owned()
        } else {
            host.tags.join(" · ")
        };
        Self {
            alias: &host.alias,
            detail: format!(
                "{} · {}:{}",
                host.protocol.stable_name().to_ascii_uppercase(),
                host.address,
                host.port
            ),
            marker,
            retained: host.effective_auth_persistence() != crate::model::AuthPersistence::PerCall,
            badges: row_badges(host),
            secondary,
            test_state,
            selected,
            checked,
        }
    }
}

fn row_label(text: RichText) -> egui::Label {
    egui::Label::new(text).truncate().selectable(false)
}

/// Draws the alias and as many badges as leave it readable; a long alias truncates first.
fn alias_line(ui: &mut egui::Ui, model: &HostRowModel<'_>, catalog: &Catalog) {
    let height = ui.text_style_height(&egui::TextStyle::Body);
    ui.allocate_ui_with_layout(
        egui::vec2(ui.available_width(), height),
        egui::Layout::left_to_right(egui::Align::Center),
        |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let gap = ui.spacing().item_spacing.x;
            let room = ui.available_width() - MIN_ALIAS_WIDTH;
            let mut reserved = 0.0;
            let mut galleys = Vec::with_capacity(model.badges.len());
            for badge in &model.badges {
                let galley = ui.painter().layout_no_wrap(
                    badge.label(catalog).to_owned(),
                    egui::FontId::proportional(BADGE_SIZE),
                    Color32::PLACEHOLDER,
                );
                let width = galley.size().x + 2.0 * BADGE_PADDING_X + gap;
                if reserved + width > room {
                    break;
                }
                reserved += width;
                galleys.push(galley);
            }
            ui.scope(|ui| {
                ui.set_max_width((ui.available_width() - reserved).max(0.0));
                ui.add(row_label(RichText::new(model.alias).strong()));
            });
            for galley in galleys {
                badge_pill(ui, galley);
            }
        },
    );
}

/// The text is centred on its painted glyph bounds, as in `tag_chip`, because the CJK
/// fallback fonts' tall line boxes would otherwise push it below the pill's middle.
fn badge_pill(ui: &mut egui::Ui, galley: std::sync::Arc<egui::Galley>) {
    let size = egui::vec2(galley.size().x + 2.0 * BADGE_PADDING_X, BADGE_HEIGHT);
    let (rect, _) = ui.allocate_exact_size(size, egui::Sense::hover());
    let color = ui.visuals().text_color().gamma_multiply(0.85);
    ui.painter().rect_stroke(
        rect,
        BADGE_HEIGHT / 2.0,
        egui::Stroke::new(1.0, color.gamma_multiply(0.5)),
        egui::StrokeKind::Inside,
    );
    let pos = rect.center() - galley.mesh_bounds.center().to_vec2();
    ui.painter().galley(pos, galley, color);
}

/// Draws one host row and returns the row response plus the checkbox state if it changed.
pub(super) fn host_row(
    ui: &mut egui::Ui,
    model: &HostRowModel<'_>,
    height: f32,
    catalog: &Catalog,
) -> (egui::Response, Option<bool>) {
    let width = ui.available_width();
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, height), egui::Sense::click());
    let visuals = ui.visuals().clone();
    let painter = ui.painter().clone();
    let accent = visuals.selection.stroke.color;
    let fill = host_row_fill(model.test_state, visuals.dark_mode).unwrap_or_else(|| {
        if model.selected {
            super::theme::accent_bar_fill(&visuals)
        } else if response.hovered() {
            visuals.widgets.hovered.weak_bg_fill
        } else {
            Color32::TRANSPARENT
        }
    });
    if fill != Color32::TRANSPARENT {
        painter.rect_filled(rect, 0.0, fill);
    }
    if model.selected {
        let bar = egui::Rect::from_min_size(rect.min, egui::vec2(ROW_ACCENT_WIDTH, rect.height()));
        painter.rect_filled(bar, 0.0, accent);
    } else {
        painter.hline(
            (rect.left() + LIST_INSET)..=(rect.right() - LIST_INSET),
            rect.bottom() + 0.5,
            visuals.widgets.noninteractive.bg_stroke,
        );
    }
    let inner = rect.shrink2(egui::vec2(LIST_INSET, ROW_PADDING_Y));
    let mut checkbox_change = None;
    // A plain child Ui (not `scope_builder`) so the parent's cursor stays at the bottom of
    // the exact row rect instead of snapping back to the content's bottom.
    let mut content = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    {
        let ui = &mut content;
        {
            ui.spacing_mut().item_spacing = egui::vec2(8.0, ROW_LINE_GAP);
            if let Some(checked) = model.checked {
                let mut value = checked;
                if ui.checkbox(&mut value, "").changed() {
                    checkbox_change = Some(value);
                }
            }
            ui.vertical(|ui| {
                ui.set_width(ui.available_width());
                alias_line(ui, model, catalog);
                // `ui.horizontal` would reserve `interact_size.y`; allocate the exact line
                // height instead so the third line stays inside the row.
                let detail_height = line_height(ui, DETAIL_SIZE);
                ui.allocate_ui_with_layout(
                    egui::vec2(ui.available_width(), detail_height),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.spacing_mut().item_spacing.x = 6.0;
                        let mut markers = Vec::with_capacity(2);
                        if let Some(marker) = model.marker {
                            markers.push(match marker {
                                RowMarker::Verified => ("✓", super::theme::success_color(&visuals)),
                                RowMarker::Testing => ("…", visuals.weak_text_color()),
                            });
                        }
                        if model.retained {
                            markers.push((RETAINED_GLYPH, visuals.warn_fg_color));
                        }
                        // Reserve the markers' width first so a long endpoint truncates instead
                        // of pushing them out of the row.
                        let reserved = markers
                            .iter()
                            .map(|(glyph, _)| {
                                let font = egui::FontId::proportional(DETAIL_SIZE);
                                ui.painter()
                                    .layout_no_wrap((*glyph).to_owned(), font, Color32::PLACEHOLDER)
                                    .size()
                                    .x
                                    + ui.spacing().item_spacing.x
                            })
                            .sum::<f32>();
                        // Between the alias (full strength) and the tags (weak) in emphasis.
                        let detail_color = visuals.text_color().gamma_multiply(0.85);
                        ui.scope(|ui| {
                            ui.set_max_width((ui.available_width() - reserved).max(0.0));
                            ui.add(row_label(
                                RichText::new(&model.detail)
                                    .size(DETAIL_SIZE)
                                    .color(detail_color),
                            ));
                        });
                        for (glyph, color) in markers {
                            ui.add(row_label(
                                RichText::new(glyph).size(DETAIL_SIZE).color(color),
                            ));
                        }
                    },
                );
                if !model.secondary.is_empty() {
                    ui.add(row_label(
                        RichText::new(&model.secondary).size(SECONDARY_SIZE).weak(),
                    ));
                }
            });
        }
    }
    drop(content);
    (response, checkbox_change)
}

impl HostsApp {
    pub(super) fn host_list(&mut self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            ui.add_space(LIST_INSET);
            ui.label(
                RichText::new(format!(
                    "{} ({})",
                    self.catalog.text("nav_title"),
                    self.store.hosts.len()
                ))
                .heading(),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add_space(LIST_INSET);
                if ui
                    .button(format!("＋ {}", self.catalog.text("new_host")))
                    .clicked()
                {
                    self.new_host();
                }
            });
        });
        ui.add_space(8.0);
        self.filter_row(ui);
        ui.add_space(8.0);
        retain_visible_selection(
            &self.store.hosts,
            &self.host_filter,
            &mut self.batch_selected,
        );
        let hosts = self
            .store
            .hosts
            .iter()
            .filter(|host| self.host_filter.matches(host))
            .collect::<Vec<_>>();
        if hosts.is_empty() {
            ui.add_space(12.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new(self.catalog.text("no_matching_hosts")).weak());
            });
        }
        let selected_id = self.selected;
        let batch_mode = self.batch_mode;
        let batch_selected = &self.batch_selected;
        let test_states = &self.test_states;
        let delete_label = self.catalog.text("delete").to_owned();
        let test_label = self.catalog.text("test_connection").to_owned();
        let retained_hint = self.catalog.text("persistence_retained_hint").to_owned();
        let mut selection_request = None;
        let mut deletion_request = None;
        let mut test_request = None;
        let mut batch_toggle_request = None;
        let three_lines = hosts
            .iter()
            .any(|host| !host.tags.is_empty() || !host.description.trim().is_empty());
        let row_height = host_row_height(ui, three_lines);
        ui.spacing_mut().item_spacing.y = 1.0;
        let mut scroll_area = egui::ScrollArea::vertical()
            .id_salt("host_list_scroll")
            .auto_shrink([false, false]);
        let mut reveal_offset = None;
        // On the first frame egui reports a placeholder (huge) height; wait for a real one.
        let viewport = ui.available_height();
        if self.scroll_to_selected && viewport > 4000.0 {
            ui.ctx().request_repaint();
        } else if self.scroll_to_selected
            && let Some(index) = hosts.iter().position(|host| Some(host.id) == selected_id)
        {
            // Rows are uniform, so the offset is known without laying the list out first.
            let pitch = row_height + ui.spacing().item_spacing.y;
            let offset = (index as f32 * pitch - (viewport - row_height) / 2.0).max(0.0);
            scroll_area = scroll_area.vertical_scroll_offset(offset);
            reveal_offset = Some(offset);
        }
        let scroll_output = scroll_area.show_rows(ui, row_height, hosts.len(), |ui, row_range| {
            ui.set_width(ui.available_width());
            for row in row_range {
                let host = &hosts[row];
                let id = host.id;
                let model = HostRowModel::from_profile(
                    host,
                    test_states.get(&id).copied(),
                    selected_id == Some(id),
                    batch_mode.then(|| batch_selected.contains(&id)),
                );
                let (response, checkbox_change) = host_row(ui, &model, row_height, &self.catalog);
                let mut hover = format!("{}\n{}", host.description, host.tags.join(", "));
                if model.retained {
                    hover = format!("{hover}\n{RETAINED_GLYPH} {retained_hint}");
                }
                for badge in &model.badges {
                    hover = format!("{hover}\n{}", badge.hint(&self.catalog));
                }
                let response = response.on_hover_text(hover);
                if let Some(checked) = checkbox_change {
                    batch_toggle_request = Some((id, checked));
                } else if response.clicked() || response.secondary_clicked() {
                    if batch_mode {
                        batch_toggle_request = Some((id, !batch_selected.contains(&id)));
                    } else if response.clicked() || selected_id != Some(id) {
                        // Right-clicking the open host keeps its unsaved form for the menu.
                        selection_request = Some(id);
                    }
                }
                if !batch_mode {
                    response.context_menu(|ui| {
                        let testable = !self.testing_all && !self.host_test_running(id);
                        if ui
                            .add_enabled(testable, egui::Button::new(&test_label))
                            .clicked()
                        {
                            test_request = Some(id);
                            ui.close();
                        }
                        if ui.button(&delete_label).clicked() {
                            deletion_request = Some(id);
                            ui.close();
                        }
                    });
                }
            }
        });
        if let Some(offset) = reveal_offset {
            // The requested offset is clamped to what the content allows; either outcome
            // means the selected row is now as visible as it can be.
            let max_offset =
                (scroll_output.content_size.y - scroll_output.inner_rect.height()).max(0.0);
            if (scroll_output.state.offset.y - offset.min(max_offset)).abs() < 1.0 {
                self.scroll_to_selected = false;
            } else {
                ui.ctx().request_repaint();
            }
        }

        if let Some((id, checked)) = batch_toggle_request {
            if checked {
                self.batch_selected.insert(id);
            } else {
                self.batch_selected.remove(&id);
            }
        } else if let Some(id) = deletion_request {
            self.select(id);
            self.delete_prompt = true;
        } else if let Some(id) = test_request {
            // Right-clicking already selected the row; test it like the editor's own button.
            if self.selected != Some(id) {
                self.select(id);
            }
            self.start_test();
        } else if let Some(id) = selection_request {
            self.select(id);
        }
    }

    fn filter_row(&mut self, ui: &mut egui::Ui) {
        let mut available_tags = normalize_tags(
            self.store
                .hosts
                .iter()
                .flat_map(|host| host.tags.iter())
                .chain(self.host_filter.tags.iter()),
        );
        available_tags.sort_by_key(|tag| tag.to_lowercase());
        let filter_active =
            !self.host_filter.search.is_empty() || !self.host_filter.tags.is_empty();
        ui.horizontal(|ui| {
            // The search box text sits at LIST_INSET like the title and rows (TextEdit margin 4).
            ui.add_space(LIST_INSET - 4.0);
            let compact = ui.available_width() < 280.0;
            let combo_width = if compact { 64.0 } else { 96.0 };
            let clear_width = if filter_active { 30.0 } else { 0.0 };
            let spacing = ui.spacing().item_spacing.x;
            let search_width =
                (ui.available_width() - combo_width - clear_width - spacing * 2.0 - LIST_INSET)
                    .max(80.0);
            let margin = super::theme::input_margin(ui);
            ui.add(
                egui::TextEdit::singleline(&mut self.host_filter.search)
                    .hint_text(self.catalog.text("search_hosts"))
                    .desired_width(search_width)
                    .margin(margin)
                    .vertical_align(egui::Align::Center),
            );
            let tag_count = self.host_filter.tags.len();
            egui::ComboBox::from_id_salt("host_tag_filter")
                .wrap_mode(egui::TextWrapMode::Truncate)
                .selected_text(if compact {
                    format!("#{tag_count}")
                } else if tag_count == 0 {
                    self.catalog.text("tags").to_owned()
                } else {
                    format!("{} ({tag_count})", self.catalog.text("tags"))
                })
                .width(combo_width)
                .show_ui(ui, |ui| {
                    ui.label(RichText::new(self.catalog.text("filter_tags")).weak());
                    ui.separator();
                    if available_tags.is_empty() {
                        ui.add_enabled(
                            false,
                            egui::Label::new(self.catalog.text("no_matching_hosts")),
                        );
                    }
                    for tag in &available_tags {
                        let mut checked = self
                            .host_filter
                            .tags
                            .iter()
                            .any(|selected| selected.to_lowercase() == tag.to_lowercase());
                        if ui.checkbox(&mut checked, tag).changed() {
                            if checked {
                                self.host_filter.tags.push(tag.clone());
                            } else {
                                self.host_filter.tags.retain(|selected| {
                                    selected.to_lowercase() != tag.to_lowercase()
                                });
                            }
                        }
                    }
                });
            if filter_active
                && ui
                    .button("×")
                    .on_hover_text(self.catalog.text("clear_filters"))
                    .clicked()
            {
                self.host_filter = HostFilter::default();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::super::tests::{find_text_rect, in_isolated_store, metadata_test_app};
    use super::*;
    use crate::model::{HostProfile, SshAuth, SuSettings, SudoSettings};
    use uuid::Uuid;

    fn list_frame(
        app: &mut HostsApp,
        context: &egui::Context,
        width: f32,
        events: Vec<egui::Event>,
    ) -> egui::FullOutput {
        context.run_ui(
            egui::RawInput {
                screen_rect: Some(egui::Rect::from_min_size(
                    egui::Pos2::ZERO,
                    egui::vec2(width, 400.0),
                )),
                events,
                ..Default::default()
            },
            |ui| app.host_list(ui),
        )
    }

    fn click(pos: egui::Pos2, button: egui::PointerButton) -> Vec<egui::Event> {
        vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton {
                pos,
                button,
                pressed: true,
                modifiers: Default::default(),
            },
            egui::Event::PointerButton {
                pos,
                button,
                pressed: false,
                modifiers: Default::default(),
            },
        ]
    }

    /// Right-clicks the row at `alias` and returns the frame showing its context menu.
    fn open_menu(app: &mut HostsApp, context: &egui::Context, alias: &str) -> egui::FullOutput {
        let shown = list_frame(app, context, 340.0, vec![]);
        let row = find_text_rect(&shown.shapes, alias).unwrap().center();
        list_frame(
            app,
            context,
            340.0,
            click(row, egui::PointerButton::Secondary),
        );
        list_frame(app, context, 340.0, vec![])
    }

    fn all_badges_profile() -> HostProfile {
        let mut host = HostProfile {
            alias: "example".into(),
            address: "server.example.com".into(),
            port: 22,
            jump_host: Some(Uuid::new_v4()),
            ..Default::default()
        };
        host.advanced.sudo = Some(SudoSettings {
            enabled: true,
            credentials: vec![],
        });
        host.advanced.su = Some(SuSettings { enabled: true });
        host.advanced.require_call_approval = true;
        host.advanced.codex_hidden = true;
        host
    }

    #[test]
    fn badges_follow_enabled_features_and_ssh_only_ones_skip_telnet() {
        assert!(row_badges(&HostProfile::default()).is_empty());
        let mut host = all_badges_profile();
        assert_eq!(
            row_badges(&host),
            [
                RowBadge::Sudo,
                RowBadge::Su,
                RowBadge::CallApproval,
                RowBadge::Jump,
                RowBadge::CodexHidden
            ]
        );
        host.advanced.sudo = Some(SudoSettings {
            enabled: false,
            credentials: vec![],
        });
        assert!(!row_badges(&host).contains(&RowBadge::Sudo));
        host.protocol = Protocol::Telnet;
        assert_eq!(row_badges(&host), [RowBadge::CodexHidden]);
    }

    #[test]
    fn badges_sit_after_the_alias_inside_the_row_in_every_locale() {
        for locale in ["en", "zh-CN", "zh-TW", "ja"] {
            let context = egui::Context::default();
            super::super::theme::configure_fonts(&context, locale);
            let mut app = metadata_test_app(&context, locale, all_badges_profile());
            let mut output = list_frame(&mut app, &context, 340.0, vec![]);
            for _ in 0..2 {
                output = list_frame(&mut app, &context, 340.0, vec![]);
            }
            let rect = |text: &str| {
                find_text_rect(&output.shapes, text)
                    .unwrap_or_else(|| panic!("{locale} is missing badge {text}"))
            };
            let alias = rect("example");
            let detail = rect("SSH · server.example.com:22");
            let mut left = alias.right();
            for badge in ["sudo", "su"].into_iter().chain(
                ["badge_call_approval", "badge_jump", "badge_codex_hidden"]
                    .map(|key| app.catalog.text(key)),
            ) {
                let badge = rect(badge);
                assert!(badge.left() >= left, "{locale}: {badge:?}");
                assert!(badge.right() <= 340.0, "{locale}: {badge:?}");
                assert!(badge.center().y < detail.top(), "{locale}: {badge:?}");
                left = badge.right();
            }
            assert!(alias.bottom() <= detail.top());
        }
    }

    #[test]
    fn narrow_rows_keep_the_alias_and_drop_the_badges_that_do_not_fit() {
        let context = egui::Context::default();
        super::super::theme::configure_fonts(&context, "en");
        let mut app = metadata_test_app(&context, "en", all_badges_profile());
        let mut output = list_frame(&mut app, &context, 150.0, vec![]);
        for _ in 0..2 {
            output = list_frame(&mut app, &context, 150.0, vec![]);
        }
        let alias = find_text_rect(&output.shapes, "example").unwrap();
        assert!(alias.width() > 30.0, "alias stays readable: {alias:?}");
        assert!(find_text_rect(&output.shapes, "sudo").is_some());
        assert!(find_text_rect(&output.shapes, app.catalog.text("badge_codex_hidden")).is_none());
    }

    #[test]
    fn right_click_keeps_the_open_draft_and_offers_test_connection() {
        let context = egui::Context::default();
        super::super::theme::configure_fonts(&context, "en");
        let mut app = metadata_test_app(&context, "en", all_badges_profile());
        app.selected = app.store.hosts.first().map(|host| host.id);
        app.editor.as_mut().unwrap().profile.description = "unsaved draft".into();
        let output = open_menu(&mut app, &context, "example");
        assert_eq!(
            app.editor.as_ref().unwrap().profile.description,
            "unsaved draft"
        );
        let test = find_text_rect(&output.shapes, app.catalog.text("test_connection")).unwrap();
        let delete = find_text_rect(&output.shapes, app.catalog.text("delete")).unwrap();
        assert!(
            test.bottom() <= delete.top(),
            "test comes first in the menu"
        );
    }

    #[test]
    fn right_click_test_connection_is_disabled_during_test_all() {
        let context = egui::Context::default();
        super::super::theme::configure_fonts(&context, "en");
        let mut app = metadata_test_app(&context, "en", all_badges_profile());
        app.selected = app.store.hosts.first().map(|host| host.id);
        app.testing_all = true;
        let output = open_menu(&mut app, &context, "example");
        let test = find_text_rect(&output.shapes, app.catalog.text("test_connection")).unwrap();
        list_frame(
            &mut app,
            &context,
            340.0,
            click(test.center(), egui::PointerButton::Primary),
        );
        assert!(app.test_operations.is_empty());
    }

    #[test]
    fn right_click_test_connection_starts_a_test_of_that_host() {
        if !in_isolated_store(
            "ui::host_list::tests::right_click_test_connection_starts_a_test_of_that_host",
        ) {
            return;
        }
        let context = egui::Context::default();
        super::super::theme::configure_fonts(&context, "en");
        let other = HostProfile::new("other".into());
        let target = HostProfile {
            alias: "target".into(),
            address: "127.0.0.1".into(),
            port: 1,
            username: "tester".into(),
            ssh_auth: SshAuth::SshAgent,
            ..Default::default()
        };
        let id = target.id;
        let mut app = metadata_test_app(&context, "en", other);
        app.store.hosts.push(target);
        app.store.save().unwrap();
        let output = open_menu(&mut app, &context, "target");
        assert_eq!(app.selected, Some(id), "right-click selects the row");
        let test = find_text_rect(&output.shapes, app.catalog.text("test_connection")).unwrap();
        list_frame(
            &mut app,
            &context,
            340.0,
            click(test.center(), egui::PointerButton::Primary),
        );
        assert!(app.test_operations.contains_key(&id));
        assert_eq!(app.test_states.get(&id), Some(&HostTestState::Testing));
    }

    #[test]
    fn host_row_keeps_alias_details_and_tags_on_separate_lines() {
        let context = egui::Context::default();
        super::super::theme::configure_fonts(&context, "en");
        let profile = HostProfile {
            alias: "example".into(),
            address: "server.example.com".into(),
            port: 22,
            verified: true,
            tags: vec!["prod".into(), "web".into()],
            ..Default::default()
        };
        let mut app = metadata_test_app(&context, "en", profile);
        for frame in 0..3 {
            let output = context.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(340.0, 400.0),
                    )),
                    time: Some(frame as f64 * 0.1),
                    ..Default::default()
                },
                |ui| app.host_list(ui),
            );
            if frame > 0 {
                let rect = |text: &str| {
                    find_text_rect(&output.shapes, text)
                        .unwrap_or_else(|| panic!("missing host row text {text}"))
                };
                let alias = rect("example");
                let detail = rect("SSH · server.example.com:22");
                let marker = rect("✓");
                let tags = rect("prod · web");
                assert!((marker.center().y - detail.center().y).abs() < 2.0);
                assert!(marker.left() >= detail.right());
                assert!(alias.bottom() <= detail.top());
                assert!(detail.bottom() <= tags.top());
                assert!(tags.right() <= 340.0);
            }
        }
    }

    #[test]
    fn test_rows_use_requested_result_colors() {
        assert_eq!(
            host_row_fill(Some(HostTestState::Succeeded), true),
            Some(TEST_SUCCESS_FILL_DARK)
        );
        assert_eq!(
            host_row_fill(Some(HostTestState::Failed), true),
            Some(TEST_FAILURE_FILL_DARK)
        );
        assert_eq!(
            host_row_fill(Some(HostTestState::Succeeded), false),
            Some(TEST_SUCCESS_FILL_LIGHT)
        );
        assert_eq!(
            host_row_fill(Some(HostTestState::Failed), false),
            Some(TEST_FAILURE_FILL_LIGHT)
        );
        assert_eq!(host_row_fill(Some(HostTestState::Testing), true), None);
        assert_eq!(host_row_fill(Some(HostTestState::Testing), false), None);
        assert_eq!(host_row_fill(None, false), None);
    }

    #[test]
    fn host_row_height_follows_line_count() {
        let context = egui::Context::default();
        super::super::theme::configure_fonts(&context, "en");
        let _ = context.run_ui(egui::RawInput::default(), |ui| {
            let two = host_row_height(ui, false);
            let three = host_row_height(ui, true);
            let body = ui.text_style_height(&egui::TextStyle::Body);
            assert!(two > body * 2.0);
            assert!(three > two + SECONDARY_SIZE * 0.8);
        });
    }

    #[test]
    fn rows_fall_back_to_the_description_when_there_are_no_tags() {
        let host = HostProfile {
            description: "first line\nsecond".into(),
            ..Default::default()
        };
        let model = HostRowModel::from_profile(&host, None, false, None);
        assert_eq!(model.secondary, "first line");
        let tagged = HostProfile {
            tags: vec!["a".into(), "b".into()],
            description: "ignored".into(),
            ..Default::default()
        };
        assert_eq!(
            HostRowModel::from_profile(&tagged, None, false, None).secondary,
            "a · b"
        );
    }
}
