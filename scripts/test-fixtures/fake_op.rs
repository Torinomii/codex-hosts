//! Offline stand-in for op.exe. Each test copies it beside its own fixture;
//! no desktop app, network, account configuration or real vault is consulted.

use std::fs::{self, OpenOptions};
use std::io::{self, Read, Write};

use serde_json::{Value, json};

fn invalid(message: &str) -> io::Error {
    io::Error::other(message)
}

fn summaries(items: &Value) -> Value {
    let Some(items) = items.as_array() else {
        return items.clone();
    };
    Value::Array(
        items
            .iter()
            .map(|item| {
                let mut summary = item.clone();
                if let Some(object) = summary.as_object_mut() {
                    let username = object
                        .get("fields")
                        .and_then(Value::as_array)
                        .and_then(|fields| {
                            fields.iter().find(|field| field["purpose"] == "USERNAME")
                        })
                        .and_then(|field| field.get("value"))
                        .cloned()
                        .unwrap_or(json!(""));
                    object.insert("additional_information".into(), username);
                    object.remove("fields");
                    object.remove("sections");
                }
                summary
            })
            .collect(),
    )
}

fn main() {
    if let Err(error) = run() {
        eprintln!("fake-op: {error}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), Box<dyn std::error::Error>> {
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .ok_or_else(|| invalid("missing fixture directory"))?;
    let path = dir.join("fake-op.json");
    let mut state: Value = serde_json::from_slice(&fs::read(&path)?)?;
    let args: Vec<String> = std::env::args().skip(1).collect();
    let words: Vec<&str> = args.iter().map(String::as_str).collect();
    let mut payload = Vec::new();
    if words.last() == Some(&"-") {
        io::stdin().read_to_end(&mut payload)?;
    }
    let mut log = OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("fake-op-calls.jsonl"))?;
    serde_json::to_writer(
        &mut log,
        &json!({
            "args": args,
            "env_names": std::env::vars_os().map(|(name, _)| name.to_string_lossy().into_owned()).collect::<Vec<_>>(),
            "stdin_len": payload.len(),
        }),
    )?;
    writeln!(log)?;
    if let Some(delay) = state["delay_ms"].as_u64() {
        std::thread::sleep(std::time::Duration::from_millis(delay));
    }

    let failures = state["failures"]
        .as_array_mut()
        .ok_or_else(|| invalid("missing fixture failures"))?;
    if !failures.is_empty() {
        let message = failures
            .remove(0)
            .as_str()
            .ok_or_else(|| invalid("invalid fixture failure"))?
            .to_owned();
        fs::write(&path, serde_json::to_vec(&state)?)?;
        return Err(invalid(&message).into());
    }

    let account = state["account"]["account_uuid"]
        .as_str()
        .ok_or_else(|| invalid("missing fixture account"))?
        .to_owned();
    let vault = state["vault"]["id"]
        .as_str()
        .ok_or_else(|| invalid("missing fixture vault"))?
        .to_owned();
    let mut changed = false;
    let output = match words.as_slice() {
        [
            "item",
            "get",
            "--vault",
            v,
            "--format",
            "json",
            "--account",
            a,
            "-",
        ] if *v == vault && *a == account => {
            let requested: Vec<Value> = serde_json::from_slice(&payload)?;
            let items = state["items"]
                .as_array()
                .ok_or_else(|| invalid("missing fixture items"))?;
            let selected = requested
                .iter()
                .map(|request| {
                    items
                        .iter()
                        .find(|item| item["id"] == request["id"])
                        .cloned()
                        .ok_or_else(|| invalid("isn't an item"))
                })
                .collect::<Result<Vec<_>, _>>()?;
            if state["stream"].as_bool() == Some(true) {
                for item in selected {
                    println!("{item}");
                }
                return Ok(());
            }
            Value::Array(selected)
        }
        ["--version"] => {
            println!("2.41.1");
            return Ok(());
        }
        ["account", "list", "--format", "json"] => json!([state["account"]]),
        ["vault", "list", "--format", "json", "--account", a] if *a == account => {
            json!([state["vault"]])
        }
        ["vault", "create", name, "--format", "json", "--account", a] if *a == account => {
            json!({"id": vault, "name": name})
        }
        [
            "item",
            "list",
            "--vault",
            v,
            "--format",
            "json",
            "--account",
            a,
        ] if *v == vault && *a == account => summaries(&state["items"]),
        [
            "item",
            "get",
            id,
            "--vault",
            v,
            "--format",
            "json",
            "--account",
            a,
        ] if *v == vault && *a == account => state["items"]
            .as_array()
            .and_then(|items| items.iter().find(|item| item["id"] == *id))
            .cloned()
            .ok_or_else(|| invalid("isn't an item"))?,
        [
            "item",
            "create",
            "--vault",
            v,
            "--format",
            "json",
            "--account",
            a,
            "-",
        ] if *v == vault && *a == account => {
            let mut item: Value = serde_json::from_slice(&payload)?;
            item["id"] = state["new_item_id"].clone();
            item["vault"] = state["vault"].clone();
            item["version"] = json!(1);
            state["items"]
                .as_array_mut()
                .ok_or_else(|| invalid("missing fixture items"))?
                .push(item.clone());
            changed = true;
            item
        }
        [
            "item",
            "edit",
            id,
            "--vault",
            v,
            "--format",
            "json",
            "--account",
            a,
            "-",
        ] if *v == vault && *a == account => {
            let mut item: Value = serde_json::from_slice(&payload)?;
            let current = state["items"]
                .as_array_mut()
                .and_then(|items| items.iter_mut().find(|item| item["id"] == *id))
                .ok_or_else(|| invalid("isn't an item"))?;
            item["version"] = json!(
                current["version"]
                    .as_u64()
                    .ok_or_else(|| invalid("missing item version"))?
                    + 1
            );
            *current = item.clone();
            changed = true;
            item
        }
        [
            "item",
            "delete",
            id,
            "--vault",
            v,
            "--format",
            "json",
            "--account",
            a,
            "--archive",
        ] if *v == vault && *a == account => {
            let items = state["items"]
                .as_array_mut()
                .ok_or_else(|| invalid("missing fixture items"))?;
            let index = items
                .iter()
                .position(|item| item["id"] == *id)
                .ok_or_else(|| invalid("isn't an item"))?;
            items.remove(index);
            changed = true;
            Value::Null
        }
        _ => return Err(invalid("unsupported command or account/vault scope").into()),
    };
    if changed {
        fs::write(&path, serde_json::to_vec(&state)?)?;
    }
    println!("{output}");
    Ok(())
}
