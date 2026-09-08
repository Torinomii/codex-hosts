#![cfg_attr(windows, windows_subsystem = "windows")]

mod connection;
mod credentials;
mod fido;
mod i18n;
mod import;
mod model;
mod ssh;
mod storage;
mod telnet;
mod temporary_secrets;
mod tool;
mod tray;
mod ui;

use std::path::PathBuf;

use eframe::egui;
use model::{Prefill, Protocol, SshAuth};

fn main() {
    match parse_args(std::env::args().skip(1).collect()) {
        Ok(LaunchMode::Gui(options)) => launch_gui(options),
        Ok(LaunchMode::TemporarySecrets(session)) => {
            let _ = session;
            if !temporary_secrets::restore_existing(true) {
                launch_gui(ui::LaunchOptions {
                    show_temporary: true,
                    ..Default::default()
                });
            }
        }
        Ok(LaunchMode::Tool {
            request_path,
            result_path,
        }) => std::process::exit(tool::run(&request_path, &result_path)),
        Err(_) => std::process::exit(2),
    }
}

fn launch_gui(options: ui::LaunchOptions) {
    if !options.codex_edit && temporary_secrets::restore_existing(options.show_temporary) {
        return;
    }
    let preferred_locale = storage::HostStore::load_recovering()
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
            Ok(Box::new(ui::HostsApp::new(creation_context, options)?))
        }),
    );
}

enum LaunchMode {
    TemporarySecrets(uuid::Uuid),
    Gui(ui::LaunchOptions),
    Tool {
        request_path: PathBuf,
        result_path: PathBuf,
    },
}

fn parse_args(args: Vec<String>) -> Result<LaunchMode, String> {
    if args.first().is_some_and(|arg| arg == "--temporary-secrets") {
        if args.len() != 2 {
            return Err("temporary secrets requires only a session UUID".into());
        }
        return uuid::Uuid::parse_str(&args[1])
            .map(LaunchMode::TemporarySecrets)
            .map_err(|_| "invalid session UUID".into());
    }
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
                        "private-key" | "private_key" | "fido" | "fido-handle"
                        | "fido_handle" => SshAuth::PrivateKey,
                        "ssh-agent" | "ssh_agent" | "agent" => SshAuth::SshAgent,
                        _ => return Err(
                            "invalid authentication method; use password, private-key/fido-handle (key file or direct FIDO handle), or ssh-agent (running SSH Agent/Pageant)"
                                .to_owned(),
                        ),
                    })
            }
            "--key-path" => {
                options.prefill.private_key_path = Some(next_value(&args, &mut index, flag)?)
            }
            "--agent-key-fingerprint" => {
                options.prefill.agent_key_fingerprint = Some(next_value(&args, &mut index, flag)?)
            }
            "--jump-host" => {
                options.prefill.jump_alias = Some(next_value(&args, &mut index, flag)?)
            }
            "--observed-fingerprint" => {
                options.observed_fingerprint = Some(next_value(&args, &mut index, flag)?)
            }
            "--observed-algorithm" => {
                options.observed_algorithm = Some(next_value(&args, &mut index, flag)?)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fido_handle_cli_alias_selects_direct_key_file_route() {
        let mode = parse_args(vec!["--auth".to_owned(), "fido-handle".to_owned()]).unwrap();
        let LaunchMode::Gui(options) = mode else {
            panic!("expected GUI launch mode");
        };
        assert_eq!(options.prefill.ssh_auth, Some(SshAuth::PrivateKey));
    }

    #[test]
    fn invalid_authentication_error_explains_agent_and_direct_fido_routes() {
        let error = parse_args(vec!["--auth".to_owned(), "hardware-key".to_owned()])
            .err()
            .expect("invalid authentication must fail");
        assert!(error.contains("fido-handle"));
        assert!(error.contains("SSH Agent/Pageant"));
    }
    #[test]
    fn temporary_mode_accepts_only_an_opaque_uuid() {
        let id = uuid::Uuid::new_v4();
        assert!(
            matches!(parse_args(vec!["--temporary-secrets".into(), id.to_string()]).unwrap(), LaunchMode::TemporarySecrets(value) if value == id)
        );
        assert!(parse_args(vec!["--temporary-secrets".into(), "not-a-session".into()]).is_err());
        assert!(
            parse_args(vec![
                "--temporary-secrets".into(),
                id.to_string(),
                "--secret".into(),
                "not-accepted".into()
            ])
            .is_err()
        );
    }
}
