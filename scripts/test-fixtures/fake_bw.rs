//! Scripted stand-in for `bw.exe`, driven by its first argument. Only the
//! plugin's integration tests use it; it is never packaged.

use std::io::{Read, Write};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(vault) = scripted_vault() {
        serve_vault(&vault, &args);
    }
    let mut stdout = std::io::stdout().lock();
    match args.first().map(String::as_str) {
        Some("--version") => writeln!(stdout, "2026.10.1").unwrap(),
        Some("echo-args") => {
            let rest: Vec<&str> = args.iter().skip(1).map(String::as_str).collect();
            writeln!(stdout, "{}", json_array(&rest)).unwrap();
        }
        Some("echo-env") => {
            let mut names: Vec<String> = std::env::vars().map(|(name, _)| name).collect();
            names.sort();
            let names: Vec<&str> = names.iter().map(String::as_str).collect();
            writeln!(stdout, "{}", json_array(&names)).unwrap();
        }
        Some("create") | Some("edit") => {
            let mut payload = Vec::new();
            std::io::stdin().read_to_end(&mut payload).unwrap();
            writeln!(stdout, "{{\"stdin_len\":{}}}", payload.len()).unwrap();
        }
        Some("flood") => {
            let chunk = vec![b'x'; 64 * 1024];
            for _ in 0..40 {
                stdout.write_all(&chunk).unwrap();
            }
        }
        Some("sleep") => {
            let millis: u64 = args
                .get(1)
                .and_then(|value| value.parse().ok())
                .unwrap_or(1000);
            std::thread::sleep(std::time::Duration::from_millis(millis));
            writeln!(stdout, "done").unwrap();
        }
        Some("login") => login(&args),
        Some("exit") => {
            let code: i32 = args
                .get(1)
                .and_then(|value| value.parse().ok())
                .unwrap_or(1);
            eprintln!("fake-bw: scripted failure");
            std::process::exit(code);
        }
        _ => {
            eprintln!("fake-bw: unknown script");
            std::process::exit(64);
        }
    }
}

const NEW_DEVICE: &str = "New device verification required. Enter OTP sent to login email:";
const TWO_STEP: &str = "Two-step login code:";

/// `login <email> --raw --passwordenv <var> [--method <n>]`, scripted by the
/// e-mail's local part and answered over stdin the way inquirer renders it.
fn login(args: &[String]) -> ! {
    if std::env::var_os("BW_NOINTERACTION").is_some() {
        fail("Code is required.");
    }
    let password_var = args
        .iter()
        .position(|arg| arg == "--passwordenv")
        .and_then(|index| args.get(index + 1));
    if password_var.and_then(std::env::var_os).is_none() {
        fail("Master password is required.");
    }
    let method = args
        .iter()
        .position(|arg| arg == "--method")
        .and_then(|index| args.get(index + 1))
        .map(String::as_str);
    let user = args
        .get(1)
        .and_then(|email| email.split('@').next())
        .unwrap_or_default();
    eprintln!("Could not find data file; creating it instead.");
    match (user, method) {
        ("device", _) => {
            let code = ask(NEW_DEVICE, true);
            if code != "246810" {
                fail("invalid new device otp");
            }
        }
        ("twostep", _) | ("methods", Some("1")) => {
            if ask(TWO_STEP, false) != "135790" {
                fail("Two-step token is invalid. Try again.");
            }
        }
        ("both", _) => {
            let _ = ask(TWO_STEP, false);
            if ask(NEW_DEVICE, false) != "246810" {
                fail("invalid new device otp");
            }
        }
        ("methods", _) => {
            eprint!(
                "\x1b[?25l? Two-step login method: (Use arrow keys)\n> Authenticator app\n  Email\n"
            );
            park();
        }
        ("odd", _) => {
            eprint!("? Favourite colour: ");
            park();
        }
        ("wait", _) => {
            eprint!("? {TWO_STEP} ");
            park();
        }
        _ => fail("Username or password is incorrect. Try again."),
    }
    println!("FAKE-SESSION");
    std::process::exit(0);
}

/// Prints a prompt (optionally in two pieces), reads one line, and echoes
/// it key by key like inquirer before ending the line.
fn ask(message: &str, split: bool) -> String {
    let mut stderr = std::io::stderr();
    let prompt = format!("? {message} ");
    if split {
        let (first, second) = prompt.split_at(prompt.len() / 2);
        write!(stderr, "{first}").unwrap();
        stderr.flush().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(100));
        write!(stderr, "{second}").unwrap();
    } else {
        write!(stderr, "{prompt}").unwrap();
    }
    stderr.flush().unwrap();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).unwrap();
    let answer = line.trim_end_matches(['\r', '\n']).to_owned();
    for end in 1..=answer.len() {
        write!(stderr, "\x1b[2K\x1b[G{prompt}{}", &answer[..end]).unwrap();
    }
    writeln!(stderr).unwrap();
    answer
}

/// Waits for input that never satisfies it, until the parent kills it.
fn park() -> ! {
    std::io::stderr().flush().unwrap();
    loop {
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).unwrap_or(0) == 0 {
            std::thread::sleep(std::time::Duration::from_secs(60));
        }
    }
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(1);
}

fn json_array(values: &[&str]) -> String {
    let quoted: Vec<String> = values
        .iter()
        .map(|value| format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\"")))
        .collect();
    format!("[{}]", quoted.join(","))
}

/// Flow tests put `fake-vault.json` in the CLI data directory: `{"session":
/// "...", "items": [<bw item JSON>, ...]}`, re-read on every call so a test
/// can edit an item between calls. Every invocation is appended to
/// `fake-bw.log` (arguments only; never the session).
fn scripted_vault() -> Option<(std::path::PathBuf, serde_json::Value)> {
    let dir = std::path::PathBuf::from(std::env::var_os("BITWARDENCLI_APPDATA_DIR")?);
    let text = std::fs::read_to_string(dir.join("fake-vault.json")).ok()?;
    Some((
        dir,
        serde_json::from_str(&text).expect("fake-vault.json is JSON"),
    ))
}

fn serve_vault((dir, vault): &(std::path::PathBuf, serde_json::Value), args: &[String]) -> ! {
    use std::fs::OpenOptions;
    if let Ok(mut log) = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("fake-bw.log"))
    {
        let _ = writeln!(log, "{}", args.join(" "));
    }
    if let Some(delay) = vault["delay_ms"].as_u64() {
        std::thread::sleep(std::time::Duration::from_millis(delay));
    }
    let session_ok = std::env::var("BW_SESSION").ok().as_deref() == vault["session"].as_str();
    let items = vault["items"].as_array().cloned().unwrap_or_default();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["--version"] => println!("2026.10.1"),
        ["status"] => println!(
            "{{\"serverUrl\":null,\"lastSync\":\"2026-11-07T00:00:00.000Z\",\"userEmail\":\"flow@example.com\",\"userId\":\"u\",\"status\":\"{}\"}}",
            if session_ok { "unlocked" } else { "locked" }
        ),
        _ if !session_ok => fail("Vault is locked."),
        ["sync"] => println!("Syncing complete."),
        ["lock"] => println!("Your vault is locked."),
        ["list", "items", ..] => println!("{}", serde_json::Value::Array(items)),
        ["get", "item", id] => match items.iter().find(|item| item["id"] == *id) {
            Some(item) => println!("{item}"),
            None => fail("Not found."),
        },
        _ => fail("fake-bw: unsupported vault command"),
    }
    std::process::exit(0);
}
