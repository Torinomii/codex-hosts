use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{LazyLock, mpsc};
use std::time::{Duration, Instant};

use eframe::egui::{self, RichText};
use semver::Version;
use serde::Deserialize;

use crate::i18n::Catalog;

const CURRENT_LABEL: &str = concat!("v", env!("CARGO_PKG_VERSION"));
const PROJECT: &str = env!("CARGO_PKG_REPOSITORY");
const RELEASES: &str = concat!(env!("CARGO_PKG_REPOSITORY"), "/releases/latest");
const BUNDLED_SKILL: &str = include_str!("../../skill/codex-hosts/VERSION");
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const RETRY_INTERVAL: Duration = Duration::from_secs(60 * 60);
static CURRENT: LazyLock<Version> = LazyLock::new(|| {
    Version::parse(env!("CARGO_PKG_VERSION")).expect("Cargo package version must be semver")
});

#[derive(Default, PartialEq, Debug)]
enum InstalledSkill {
    #[default]
    Missing,
    Unversioned,
    Version(Version),
}

impl InstalledSkill {
    fn at(dir: &Path) -> Self {
        if !dir.join("SKILL.md").is_file() {
            return Self::Missing;
        }
        fs::read_to_string(dir.join("VERSION"))
            .ok()
            .and_then(|text| Version::parse(text.trim()).ok())
            .map_or(Self::Unversioned, Self::Version)
    }

    fn detect() -> Self {
        let home = env::var_os("CODEX_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .or_else(|| env::home_dir().map(|home| home.join(".codex")));
        Self::for_executable(env::current_exe().ok().as_deref(), home.as_deref())
    }

    fn for_executable(exe: Option<&Path>, home: Option<&Path>) -> Self {
        if let Some(bin) = exe.and_then(Path::parent)
            && bin.ends_with("bin")
            && let Some(root) = bin.parent()
        {
            // Linked Skill entry point, or the executable in an extracted release.
            if root.ends_with("skills/codex-hosts")
                || root.ends_with("skill/codex-hosts")
                || (root.ends_with("codex-hosts") && root.join("SKILL.md").is_file())
            {
                return Self::at(root);
            }
            let packaged = root.join("skill/codex-hosts");
            if packaged.is_dir() {
                return Self::at(&packaged);
            }
        }
        home.map(|home| Self::at(&home.join("skills/codex-hosts")))
            .unwrap_or_default()
    }
}

fn stable_release(bytes: &[u8]) -> Option<Version> {
    #[derive(Deserialize)]
    struct Release {
        tag_name: String,
        draft: bool,
        prerelease: bool,
    }
    let release: Release = serde_json::from_slice(bytes).ok()?;
    let tag = release
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&release.tag_name);
    let version = Version::parse(tag).ok()?;
    (!release.draft && !release.prerelease && version.pre.is_empty()).then_some(version)
}

fn latest_release() -> Option<Version> {
    let project = PROJECT.strip_prefix("https://github.com/")?;
    let endpoint = format!("https://api.github.com/repos/{project}/releases/latest");
    #[cfg(windows)]
    let mut command = {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::CREATE_NO_WINDOW;
        let mut command =
            Command::new(PathBuf::from(env::var_os("SystemRoot")?).join("System32/curl.exe"));
        command.creation_flags(CREATE_NO_WINDOW);
        command
    };
    #[cfg(not(windows))]
    let mut command = Command::new("curl");
    // Ignore curlrc so this public metadata request has fixed options and no user credentials.
    let output = command
        .args([
            "--disable",
            "--silent",
            "--fail",
            "--proto",
            "=https",
            "--connect-timeout",
            "5",
            "--max-time",
            "12",
            "--max-filesize",
            "524288",
            "--user-agent",
            "codex-hosts/version-check",
            "--header",
            "Accept: application/vnd.github+json",
            &endpoint,
        ])
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    stable_release(&output.stdout)
}

#[derive(Default)]
pub(super) struct VersionPopover {
    pub(super) open: bool,
    latest: Option<Version>,
    task: Option<mpsc::Receiver<Option<Version>>>,
    last_check: Option<Instant>,
    failed: bool,
    requested: bool,
    notified: Option<Version>,
    skill: InstalledSkill,
}

impl VersionPopover {
    pub(super) fn poll(&mut self, ctx: &egui::Context, automatic: bool) {
        if let Some(task) = &self.task {
            let result = match task.try_recv() {
                Ok(result) => result,
                Err(mpsc::TryRecvError::Empty) => return,
                Err(mpsc::TryRecvError::Disconnected) => None,
            };
            self.failed = result.is_none();
            if let Some(version) = result {
                self.latest = Some(version);
            }
            self.task = None;
        }
        let interval = if self.failed {
            RETRY_INTERVAL
        } else {
            CHECK_INTERVAL
        };
        let remaining = self
            .last_check
            .map(|last| interval.saturating_sub(last.elapsed()))
            .unwrap_or_default();
        if self.requested || (automatic && remaining.is_zero()) {
            self.requested = false;
            self.last_check = Some(Instant::now());
            let (send, receive) = mpsc::channel();
            self.task = Some(receive);
            let repaint = ctx.clone();
            std::thread::spawn(move || {
                let _ = send.send(latest_release());
                repaint.request_repaint();
            });
        } else if automatic {
            ctx.request_repaint_after(remaining);
        }
    }

    fn has_update(&self) -> bool {
        self.latest
            .as_ref()
            .is_some_and(|version| version.cmp_precedence(&CURRENT).is_gt())
    }

    pub(super) fn show(&mut self, ui: &mut egui::Ui, catalog: &Catalog, notify: bool) {
        let newer = self.has_update();
        let text = RichText::new(CURRENT_LABEL);
        let text = if newer {
            text.color(ui.visuals().hyperlink_color)
        } else {
            text
        };
        let interactive = ui.memory(|memory| memory.allows_interaction(ui.layer_id()));
        let cell_size = egui::vec2(80.0, ui.available_height());
        let button = ui
            .add_enabled(
                interactive,
                egui::Button::new(text)
                    .min_size(cell_size)
                    .corner_radius(0)
                    .frame_when_inactive(false),
            )
            .on_hover_cursor(egui::CursorIcon::PointingHand);
        ui.painter().vline(
            button.rect.left(),
            button.rect.y_range(),
            ui.visuals().widgets.noninteractive.bg_stroke,
        );
        if !button.enabled() {
            self.open = false;
            return;
        }
        let auto_open =
            notify && newer && self.latest != self.notified && !egui::Popup::is_any_open(ui.ctx());
        let was_open = self.open;
        if button.clicked() {
            self.open = !self.open;
        } else if auto_open {
            self.open = true;
        }
        if self.open && newer {
            self.notified.clone_from(&self.latest);
        }
        if !self.open {
            button.on_hover_text(catalog.text("version_title"));
            return;
        }
        if !was_open {
            self.skill = InstalledSkill::detect();
            self.requested |= self.last_check.is_none();
            ui.ctx().request_repaint();
        }

        let frame = egui::Frame::popup(ui.style())
            .inner_margin(12)
            .corner_radius(8);
        let width = 260.0_f32.min((ui.ctx().content_rect().width() - 56.0).max(100.0));
        let mut open = self.open;
        let shown = egui::Popup::from_response(&button)
            .id(egui::Id::new("version_popover"))
            .anchor(button.rect.shrink2(egui::vec2(10.0, 0.0)))
            .align(egui::RectAlign::TOP_END)
            .align_alternatives(&[])
            .gap(10.0)
            .width(width + frame.total_margin().sum().x)
            .frame(frame)
            .open_bool(&mut open)
            .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
            .show(|ui| {
                ui.set_width(width);
                self.contents(ui, catalog, newer);
            });
        self.open = self.open && open;
        if let Some(shown) = shown {
            let panel = shown.response.rect;
            let x = button
                .rect
                .center()
                .x
                .clamp(panel.left() + 12.0, panel.right() - 12.0);
            let base = panel.bottom() - frame.stroke.width;
            let points = vec![
                egui::pos2(x - 8.0, base),
                egui::pos2(x, button.rect.top() - 1.0),
                egui::pos2(x + 8.0, base),
            ];
            let painter = ui.ctx().layer_painter(shown.response.layer_id);
            painter.add(egui::Shape::convex_polygon(
                points.clone(),
                frame.fill,
                egui::Stroke::NONE,
            ));
            painter.add(egui::Shape::line(points, frame.stroke));
        }
    }

    fn contents(&mut self, ui: &mut egui::Ui, catalog: &Catalog, newer: bool) {
        ui.horizontal(|ui| {
            ui.strong(catalog.text(if newer {
                "version_update"
            } else {
                "version_title"
            }));
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .add(
                        egui::Button::new(catalog.text("close"))
                            .small()
                            .frame_when_inactive(false),
                    )
                    .clicked()
                {
                    self.open = false;
                }
            });
        });
        ui.separator();
        egui::Grid::new("version_details")
            .spacing([14.0, 5.0])
            .show(ui, |ui| {
                ui.label(catalog.text("version_local"));
                ui.add(egui::Label::new(RichText::new(CURRENT_LABEL).monospace()).selectable(true));
                ui.end_row();
                ui.label(catalog.text("version_stable"));
                if let Some(version) = &self.latest {
                    let value = RichText::new(format!("v{version}")).monospace();
                    ui.label(if newer {
                        value.color(ui.visuals().hyperlink_color)
                    } else {
                        value
                    });
                } else {
                    ui.label(catalog.text(if self.failed && self.task.is_none() {
                        "version_unavailable"
                    } else {
                        "version_checking"
                    }));
                }
                ui.end_row();
                ui.label(catalog.text("version_skill"));
                match &self.skill {
                    InstalledSkill::Version(v) => ui.monospace(format!("v{v}")),
                    InstalledSkill::Missing => ui.label(catalog.text("version_skill_missing")),
                    InstalledSkill::Unversioned => ui.label(catalog.text("version_skill_unknown")),
                };
                ui.end_row();
                let bundled = BUNDLED_SKILL.trim();
                if !matches!(&self.skill, InstalledSkill::Version(v) if v.to_string() == bundled) {
                    ui.label(catalog.text("version_bundled"));
                    ui.monospace(format!("v{bundled}"));
                    ui.end_row();
                }
            });
        if self.failed && self.latest.is_some() {
            ui.small(catalog.text("version_cached"));
        }
        ui.add_space(5.0);
        ui.horizontal_wrapped(|ui| {
            ui.hyperlink_to(catalog.text("version_project"), PROJECT);
            if newer {
                ui.hyperlink_to(catalog.text("version_notes"), RELEASES);
            }
            if self.failed
                && self.task.is_none()
                && ui.small_button(catalog.text("version_retry")).clicked()
            {
                self.requested = true;
                ui.ctx().request_repaint();
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::tests::{click_events, find_text_rect};

    #[test]
    fn only_published_stable_tags_produce_a_version() {
        for (tag, draft, prerelease, expected) in [
            ("v0.10.2", false, false, Some(Version::new(0, 10, 2))),
            ("1.2.3", false, false, Some(Version::new(1, 2, 3))),
            ("v1.2.3", true, false, None),
            ("v1.2.3", false, true, None),
            ("v1.2.3-rc.1", false, false, None),
            ("next", false, false, None),
        ] {
            let body =
                serde_json::json!({"tag_name": tag, "draft": draft, "prerelease": prerelease});
            assert_eq!(stable_release(body.to_string().as_bytes()), expected);
        }
        assert!(stable_release(br#"{"message":"rate limited"}"#).is_none());
    }

    #[test]
    fn skill_version_comes_from_the_installed_copy_including_older_skills() {
        Version::parse(BUNDLED_SKILL.trim()).unwrap();
        let dir = env::temp_dir().join(format!(
            "codex-hosts-skill-version-{}",
            uuid::Uuid::new_v4()
        ));
        fs::create_dir(&dir).unwrap();
        assert_eq!(InstalledSkill::at(&dir), InstalledSkill::Missing);
        fs::write(dir.join("SKILL.md"), "# Test Skill").unwrap();
        assert_eq!(InstalledSkill::at(&dir), InstalledSkill::Unversioned);
        fs::write(dir.join("VERSION"), "0.3.1\r\n").unwrap();
        assert_eq!(
            InstalledSkill::at(&dir),
            InstalledSkill::Version(Version::new(0, 3, 1))
        );
        fs::write(dir.join("VERSION"), "unknown").unwrap();
        assert_eq!(InstalledSkill::at(&dir), InstalledSkill::Unversioned);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn launched_skill_takes_precedence_over_a_different_codex_home() {
        const TEST: &str =
            "ui::version::tests::launched_skill_takes_precedence_over_a_different_codex_home";
        if env::var("CODEX_HOSTS_ISOLATED_TEST").as_deref() == Ok(TEST) {
            assert_eq!(
                InstalledSkill::detect(),
                InstalledSkill::Version(Version::new(2, 0, 0))
            );
            return;
        }
        let root = env::temp_dir().join(format!("codex-hosts-version-{}", uuid::Uuid::new_v4()));
        let home = root.join("default");
        let inherited = home.join("skills/codex-hosts");
        let installed = root.join("other/skills/codex-hosts");
        let package = root.join("release/skill/codex-hosts");
        for (dir, version) in [
            (&inherited, "1.0.0"),
            (&installed, "2.0.0"),
            (&package, "3.0.0"),
        ] {
            fs::create_dir_all(dir).unwrap();
            fs::write(dir.join("SKILL.md"), "# Skill").unwrap();
            fs::write(dir.join("VERSION"), version).unwrap();
        }
        let detect = |exe: &Path| InstalledSkill::for_executable(Some(exe), Some(&home));
        let exe = installed.join("bin/codex-hosts.exe");
        assert_eq!(detect(&exe), InstalledSkill::Version(Version::new(2, 0, 0)));
        fs::create_dir(exe.parent().unwrap()).unwrap();
        let test_binary = env::current_exe().unwrap();
        #[cfg(windows)]
        if std::os::windows::fs::symlink_file(&test_binary, &exe).is_err() {
            fs::copy(&test_binary, &exe).unwrap();
        }
        #[cfg(not(windows))]
        fs::copy(&test_binary, &exe).unwrap();
        let child = Command::new(&exe)
            .args(["--exact", TEST])
            .env("CODEX_HOSTS_ISOLATED_TEST", TEST)
            .env("CODEX_HOME", &home)
            .output()
            .unwrap();
        let output = String::from_utf8_lossy(&child.stdout);
        assert!(
            child.status.success() && output.contains("1 passed"),
            "{output}"
        );
        fs::remove_file(installed.join("VERSION")).unwrap();
        assert_eq!(detect(&exe), InstalledSkill::Unversioned);
        fs::remove_file(installed.join("SKILL.md")).unwrap();
        assert_eq!(detect(&exe), InstalledSkill::Missing);
        assert_eq!(
            detect(&root.join("release/bin/codex-hosts.exe")),
            InstalledSkill::Version(Version::new(3, 0, 0))
        );
        assert_eq!(
            detect(&root.join("target/debug/codex-hosts.exe")),
            InstalledSkill::Version(Version::new(1, 0, 0))
        );
        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn only_a_newer_release_opens_once_and_points_to_its_release_notes() {
        let ctx = egui::Context::default();
        let catalog = Catalog::for_locale(Some("en"));
        let frame = |popover: &mut VersionPopover, notify, events| {
            ctx.run_ui(
                egui::RawInput {
                    screen_rect: Some(egui::Rect::from_min_size(
                        egui::Pos2::ZERO,
                        egui::vec2(820.0, 620.0),
                    )),
                    events,
                    ..Default::default()
                },
                |ui| {
                    egui::Panel::bottom("status_test").show(ui, |ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            popover.show(ui, &catalog, notify);
                        });
                    });
                },
            )
        };
        let mut view = VersionPopover {
            last_check: Some(Instant::now()),
            ..Default::default()
        };
        let mut same_precedence = CURRENT.clone();
        same_precedence.build = semver::BuildMetadata::new("local").unwrap();
        for version in [Version::new(0, 0, 0), CURRENT.clone(), same_precedence] {
            view.latest = Some(version);
            frame(&mut view, true, vec![]);
            assert!(!view.open);
        }
        let newer = Version::new(CURRENT.major + 1, 0, 0);
        view.latest = Some(newer.clone());
        frame(&mut view, false, vec![]);
        assert!(!view.open);
        for _ in 0..3 {
            frame(&mut view, true, vec![]);
        }
        let shown = frame(&mut view, true, vec![]);
        assert!(view.open);
        assert!(find_text_rect(&shown.shapes, &format!("v{newer}")).is_some());
        let link = find_text_rect(&shown.shapes, catalog.text("version_notes")).unwrap();
        let clicked = frame(&mut view, true, click_events(link.center()));
        assert!(
            clicked
                .platform_output
                .commands
                .iter()
                .any(|cmd| matches!(cmd, egui::OutputCommand::OpenUrl(url) if url.url == RELEASES))
        );
        frame(&mut view, true, click_events(egui::pos2(40.0, 40.0)));
        frame(&mut view, true, vec![]);
        assert!(!view.open);
        view.latest = Some(Version::new(newer.major, 0, 1));
        frame(&mut view, true, vec![]);
        assert!(view.open);
    }

    #[test]
    fn an_offline_recheck_keeps_the_last_known_release() {
        let (send, receive) = mpsc::channel();
        let last = Version::new(0, 3, 0);
        let mut view = VersionPopover {
            latest: Some(last.clone()),
            task: Some(receive),
            last_check: Some(Instant::now()),
            ..Default::default()
        };
        send.send(None).unwrap();
        view.poll(&egui::Context::default(), false);
        assert!(view.failed && view.task.is_none());
        assert_eq!(view.latest, Some(last));
        assert!(!view.open && !view.has_update());
    }
}
