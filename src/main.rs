#![cfg_attr(windows, windows_subsystem = "windows")]

mod connection;
mod credentials;
mod i18n;
mod import;
mod model;
mod ssh;
mod storage;
mod telnet;
mod tool;
mod ui;

use std::path::PathBuf;

use eframe::egui;
use model::{Prefill, Protocol, SshAuth};

fn main() {
    match parse_args(std::env::args().skip(1).collect()) {
        Ok(LaunchMode::Gui(options)) => launch_gui(options),
        Ok(LaunchMode::Tool {
            request_path,
            result_path,
        }) => std::process::exit(tool::run(&request_path, &result_path)),
        Err(_) => std::process::exit(2),
    }
}

fn launch_gui(options: ui::LaunchOptions) {
    let preferred_locale = storage::HostStore::load()
        .ok()
        .and_then(|store| store.preferred_locale);
    let title = i18n::Catalog::for_locale(preferred_locale.as_deref())
        .text("app_title")
        .to_owned();
    let native_options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(title)
            .with_inner_size([1040.0, 760.0])
            .with_min_inner_size([820.0, 620.0]),
        ..Default::default()
    };
    let _ = eframe::run_native(
        "Codex Hosts",
        native_options,
        Box::new(move |creation_context| {
            Ok(Box::new(ui::HostsApp::new(creation_context, options)))
        }),
    );
}

enum LaunchMode {
    Gui(ui::LaunchOptions),
    Tool {
        request_path: PathBuf,
        result_path: PathBuf,
    },
}

fn parse_args(args: Vec<String>) -> Result<LaunchMode, String> {
    let mut options = ui::LaunchOptions::default();
    let mut request_path = None;
    let mut result_path = None;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].as_str();
        match flag {
            "--codex-edit" => options.codex_edit = true,
            "--alias" => options.prefill.alias = Some(next_value(&args, &mut index, flag)?),
            "--host" => options.prefill.address = Some(next_value(&args, &mut index, flag)?),
            "--port" => {
                options.prefill.port = Some(
                    next_value(&args, &mut index, flag)?
                        .parse::<u16>()
                        .map_err(|_| "invalid port".to_owned())?,
                )
            }
            "--user" => options.prefill.username = Some(next_value(&args, &mut index, flag)?),
            "--protocol" => {
                options.prefill.protocol =
                    Some(match next_value(&args, &mut index, flag)?.as_str() {
                        "ssh" => Protocol::Ssh,
                        "telnet" => Protocol::Telnet,
                        _ => return Err("invalid protocol".to_owned()),
                    })
            }
            "--auth" => {
                options.prefill.ssh_auth =
                    Some(match next_value(&args, &mut index, flag)?.as_str() {
                        "password" => SshAuth::Password,
                        "private-key" | "private_key" => SshAuth::PrivateKey,
                        _ => return Err("invalid authentication method".to_owned()),
                    })
            }
            "--key-path" => {
                options.prefill.private_key_path = Some(next_value(&args, &mut index, flag)?)
            }
            "--jump-host" => {
                options.prefill.jump_alias = Some(next_value(&args, &mut index, flag)?)
            }
            "--observed-fingerprint" => {
                options.observed_fingerprint = Some(next_value(&args, &mut index, flag)?)
            }
            "--result-file" => {
                options.result_path = Some(PathBuf::from(next_value(&args, &mut index, flag)?))
            }
            "--tool-request" => {
                request_path = Some(PathBuf::from(next_value(&args, &mut index, flag)?))
            }
            "--tool-result" => {
                result_path = Some(PathBuf::from(next_value(&args, &mut index, flag)?))
            }
            _ => return Err(format!("unknown argument: {flag}")),
        }
        index += 1;
    }

    if request_path.is_some() || result_path.is_some() {
        return Ok(LaunchMode::Tool {
            request_path: request_path.ok_or_else(|| "missing --tool-request".to_owned())?,
            result_path: result_path.ok_or_else(|| "missing --tool-result".to_owned())?,
        });
    }
    Ok(LaunchMode::Gui(options))
}

fn next_value(args: &[String], index: &mut usize, flag: &str) -> Result<String, String> {
    *index += 1;
    args.get(*index)
        .cloned()
        .ok_or_else(|| format!("missing value for {flag}"))
}

impl From<Prefill> for ui::LaunchOptions {
    fn from(prefill: Prefill) -> Self {
        Self {
            prefill,
            ..Self::default()
        }
    }
}
