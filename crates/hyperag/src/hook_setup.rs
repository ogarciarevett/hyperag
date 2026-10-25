//! Central hook configuration and non-destructive harness setup.
use std::{collections::BTreeMap, path::{Path, PathBuf}};
use anyhow::{Context, Result, ensure};
use serde::Deserialize;
use serde_json::{Value, json};
use toml_edit::{DocumentMut, Item};

pub const EVENTS: [&str; 3] = ["session-start", "prompt", "session-end"];
pub const HARNESSES: [&str; 5] = ["claude-code", "codex", "cursor", "gemini", "kimi"];
const GEMINI_ENV: &str = "$environment-allowance";
const DEFAULT_CONFIG: &str = include_str!("hooks-default.toml");

#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub url: String,
    pub token_env: String,
    pub scope: String,
    pub timeout_ms: u64,
    pub events: BTreeMap<String, bool>,
    pub harnesses: BTreeMap<String, Harness>,
}
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Harness {
    pub enabled: bool,
    pub events: Vec<String>,
}
impl Default for Harness {
    fn default() -> Self { Self { enabled: true, events: EVENTS.map(str::to_owned).to_vec() } }
}
impl Default for Config {
    fn default() -> Self {
        Self { url: "http://127.0.0.1:7480".into(), token_env: "HYPERAG_TOKEN".into(), scope: "project".into(), timeout_ms: 1500, events: BTreeMap::new(), harnesses: BTreeMap::new() }
    }
}
pub fn home() -> Result<PathBuf> {
    std::env::var_os("HOME").filter(|v| !v.is_empty()).map(PathBuf::from).context("HOME is required")
}
pub fn config_dir(home: &Path) -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from).unwrap_or_else(|| home.join(".config")).join("hyperag")
}
impl Config {
    pub fn load(home: &Path) -> Result<Self> {
        let path = config_dir(home).join("hooks.toml");
        let config: Self = match std::fs::read_to_string(&path) {
            Ok(raw) => toml_edit::de::from_str(&raw).context("invalid hooks.toml")?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(e) => return Err(e.into()),
        };
        ensure!((100..=2000).contains(&config.timeout_ms), "timeout_ms must be 100..2000");
        ensure!(!config.token_env.is_empty() && config.token_env.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'), "token_env must name an environment variable");
        ensure!(!config.scope.is_empty(), "scope must be project or a fixed scope");
        for event in config.events.keys().chain(config.harnesses.values().flat_map(|h| h.events.iter())) {
            ensure!(EVENTS.contains(&event.as_str()), "unknown common event {event}");
        }
        for name in config.harnesses.keys() { ensure!(HARNESSES.contains(&name.as_str()), "unknown harness {name}"); }
        Ok(config)
    }
    pub fn enabled(&self, harness: &str, event: &str) -> bool {
        self.events.get(event) != Some(&false) && self.harnesses.get(harness).is_none_or(|h| h.enabled && h.events.iter().any(|e| e == event))
    }
}

fn target(home: &Path, harness: &str) -> PathBuf {
    home.join(match harness {
        "claude-code" => ".claude/settings.json",
        "codex" => ".codex/config.toml",
        "cursor" => ".cursor/hooks.json",
        "gemini" => ".gemini/settings.json",
        "kimi" => ".kimi-code/config.toml",
        _ => unreachable!(),
    })
}
fn native(harness: &str, event: &str) -> &'static str {
    match (harness, event) {
        ("cursor", "session-start") => "sessionStart",
        ("cursor", "prompt") => "beforeSubmitPrompt",
        ("cursor", "session-end") => "sessionEnd",
        ("gemini", "prompt") => "BeforeAgent",
        (_, "session-start") => "SessionStart",
        (_, "prompt") => "UserPromptSubmit",
        (_, "session-end") => "SessionEnd",
        _ => unreachable!(),
    }
}
fn quote(s: &str) -> String { format!("'{}'", s.replace('\'', "'\\''")) }
fn entry(binary: &Path, harness: &str, event: &str) -> Value {
    let command = format!("{} hook {event} --harness {harness} # hyperag-managed:v1", quote(&binary.to_string_lossy()));
    if harness == "kimi" { return json!({"event": native(harness, event), "command": command, "timeout": 3}); }
    if harness == "cursor" { return json!({"command": command, "timeout": 3}); }
    let timeout = if harness == "gemini" { 3000 } else { 3 };
    json!({"hooks": [{"type": "command", "command": command, "timeout": timeout}]})
}

// A manifest records full inserted entries. Changed user entries never match and are preserved.
type Manifest = BTreeMap<String, BTreeMap<String, Value>>;
fn read(path: &Path) -> Result<String> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(s),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(String::new()),
        Err(e) => Err(e.into()),
    }
}
fn save(path: &Path, text: &str) -> Result<()> {
    use std::io::Write;
    let old = read(path)?;
    if old == text { return Ok(()); }
    let parent = path.parent().context("path has no parent")?;
    std::fs::create_dir_all(parent)?;
    if path.exists() {
        let prefix = format!("{}.hyperag-backup-", path.file_name().unwrap().to_string_lossy());
        let mut backup = tempfile::Builder::new().prefix(&prefix).tempfile_in(parent)?;
        backup.write_all(old.as_bytes())?;
        backup.keep()?;
    }
    let mut next = tempfile::NamedTempFile::new_in(parent)?;
    if path.exists() { next.as_file().set_permissions(std::fs::metadata(path)?.permissions())?; }
    next.write_all(text.as_bytes())?;
    next.as_file().sync_all()?;
    ensure!(read(path)? == old, "{} changed during setup; retry", path.display());
    next.persist(path)?;
    Ok(())
}
fn json_merge(raw: &str, harness: &str, old: &BTreeMap<String, Value>, new: &BTreeMap<String, Value>) -> Result<String> {
    let mut doc: Value = if raw.is_empty() { json!({}) } else { serde_json::from_str(raw)? };
    let root = doc.as_object_mut().context("config must be an object")?;
    if harness == "cursor" {
        if let Some(version) = root.get("version") { ensure!(version == 1, "unsupported Cursor hooks version"); }
        if !new.is_empty() { root.entry("version").or_insert(json!(1)); }
    }
    if old.is_empty() && new.is_empty() { return Ok(raw.into()); }
    let hooks = root.entry("hooks").or_insert(json!({})).as_object_mut().context("hooks must be an object")?;
    let mut emptied = Vec::new();
    for event in old.keys().chain(new.keys()).filter(|e| e.as_str() != GEMINI_ENV) {
        if let Some(items) = hooks.get_mut(event) {
            let items = items.as_array_mut().context("hook event must be an array")?;
            let before = items.len();
            for owned in old.get(event).and_then(Value::as_array).into_iter().flatten() {
                items.retain_mut(|item| {
                    if item == owned { return false; }
                    if let (Some(expected), Some(handlers)) = (owned["hooks"].as_array(), item.get_mut("hooks").and_then(Value::as_array_mut)) {
                        let before = handlers.len();
                        handlers.retain(|h| !expected.contains(h));
                        return before == handlers.len() || !handlers.is_empty();
                    }
                    true
                });
            }
            if before > 0 && items.is_empty() { emptied.push(event.clone()); }
        }
    }
    for event in emptied { hooks.remove(&event); }
    for (event, item) in new.iter().filter(|(e, _)| e.as_str() != GEMINI_ENV) {
        let items = hooks.entry(event).or_insert(json!([])).as_array_mut().context("hook event must be an array")?;
        if !items.contains(item) { items.push(item.clone()); }
    }
    if harness == "gemini" { merge_gemini_env(root, old, new)?; }
    Ok(serde_json::to_string_pretty(&doc)? + "\n")
}
fn merge_gemini_env(root: &mut serde_json::Map<String, Value>, old: &BTreeMap<String, Value>, new: &BTreeMap<String, Value>) -> Result<()> {
    let addition = new.get(GEMINI_ENV);
    if addition.is_none() && !old.contains_key(GEMINI_ENV) { return Ok(()); }
    if addition.is_some() {
        let security = root.entry("security").or_insert(json!({})).as_object_mut().context("security must be an object")?;
        let redaction = security.entry("environmentVariableRedaction").or_insert(json!({})).as_object_mut().context("environmentVariableRedaction must be an object")?;
        redaction.entry("allowed").or_insert(json!([]));
    }
    let allowed = root.get_mut("security").and_then(|s| s.get_mut("environmentVariableRedaction")).and_then(|r| r.get_mut("allowed"));
    if let Some(allowed) = allowed {
        let allowed = allowed.as_array_mut().context("environment redaction allowed must be an array")?;
        for entry in old.get(GEMINI_ENV).and_then(Value::as_array).into_iter().flatten() {
            allowed.retain(|name| name != &entry["variable"]);
        }
        if let Some(entry) = addition {
            if !allowed.contains(&entry["variable"]) { allowed.push(entry["variable"].clone()); }
        }
    }
    Ok(())
}
fn toml_merge(raw: &str, harness: &str, old: &BTreeMap<String, Value>, new: &BTreeMap<String, Value>) -> Result<String> {
    let mut doc: DocumentMut = raw.parse()?;
    let mut emptied = Vec::new();
    for event in old.keys().chain(new.keys()) {
        let item = if harness == "kimi" { doc.get_mut("hooks") } else { doc.get_mut("hooks").and_then(Item::as_table_like_mut).and_then(|h| h.get_mut(event)) };
        if let Some(item) = item {
            let before = array_len(item);
            for owned in old.get(event).and_then(Value::as_array).into_iter().flatten() {
                remove_toml(item, owned)?;
            }
            if before > 0 && array_len(item) == 0 { emptied.push(event.clone()); }
        }
    }
    for (event, value) in new {
        let source = toml_edit::ser::to_string(value)?;
        let table = source.parse::<DocumentMut>()?.into_table();
        if harness != "kimi" {
            if let Some(inline) = doc.get_mut("hooks").and_then(Item::as_inline_table_mut) {
                let array = inline.entry(event).or_insert(toml_edit::Value::Array(Default::default())).as_array_mut().context("hook event must be an array")?;
                array.push(table.into_inline_table());
                continue;
            }
        }
        let slot = if harness == "kimi" {
            doc.entry("hooks").or_insert(Item::ArrayOfTables(Default::default()))
        } else {
            if !doc.contains_key("hooks") { doc["hooks"] = Item::Table(Default::default()); }
            doc["hooks"].as_table_like_mut().context("hooks must be a table")?.entry(event).or_insert(Item::ArrayOfTables(Default::default()))
        };
        if let Some(tables) = slot.as_array_of_tables_mut() { tables.push(table); }
        else if let Some(array) = slot.as_array_mut() { array.push(table.into_inline_table()); }
        else { anyhow::bail!("hook event must be an array of tables"); }
    }
    // Remove empty event arrays, leaving unrelated keys and comments in place.
    if harness != "kimi" {
        if let Some(hooks) = doc.get_mut("hooks").and_then(Item::as_table_like_mut) {
            for key in emptied {
                if hooks.get(&key).is_some_and(|item| array_len(item) == 0) { hooks.remove(&key); }
            }
        }
    }
    Ok(doc.to_string())
}

fn array_len(item: &Item) -> usize {
    item.as_array_of_tables().map(|a| a.len()).or_else(|| item.as_array().map(|a| a.len())).unwrap_or(0)
}
fn remove_toml(item: &mut Item, owned: &Value) -> Result<()> {
    if let Some(tables) = item.as_array_of_tables_mut() {
        for index in (0..tables.len()).rev() {
            let table = tables.get_mut(index).unwrap();
            let value = toml_edit::de::from_str::<Value>(&table.to_string()).ok();
            let mut remove = value.as_ref() == Some(owned);
            if !remove {
                if let (Some(expected), Some(handlers)) = (owned["hooks"].as_array(), table.get_mut("hooks")) {
                    let before = array_len(handlers);
                    for handler in expected { remove_toml(handlers, handler)?; }
                    remove = before > 0 && array_len(handlers) == 0;
                }
            }
            if remove { tables.remove(index); }
        }
    } else if let Some(array) = item.as_array_mut() {
        for index in (0..array.len()).rev() {
            let value = array.get_mut(index).unwrap();
            let semantic = toml_edit::de::from_str::<Value>(&format!("v = {value}")).ok().map(|doc| doc["v"].clone());
            let mut remove = semantic.as_ref() == Some(owned);
            if !remove {
                if let (Some(expected), Some(handlers)) = (owned["hooks"].as_array(), value.as_inline_table_mut().and_then(|t| t.get_mut("hooks"))) {
                    let mut wrapped = Item::Value(handlers.clone());
                    let before = array_len(&wrapped);
                    for handler in expected { remove_toml(&mut wrapped, handler)?; }
                    remove = before > 0 && array_len(&wrapped) == 0;
                    *handlers = wrapped.into_value().map_err(|_| anyhow::anyhow!("invalid hook array"))?;
                }
            }
            if remove { array.remove(index); }
        }
    } else { anyhow::bail!("hook event must be an array of tables"); }
    Ok(())
}

fn parsed(raw: &str, toml: bool) -> Result<Value> {
    if raw.is_empty() { return Ok(json!({})); }
    Ok(if toml { toml_edit::de::from_str(raw)? } else { serde_json::from_str(raw)? })
}
fn present(doc: &Value, harness: &str, event: &str, entry: &Value) -> bool {
    if event == GEMINI_ENV {
        return doc.pointer("/security/environmentVariableRedaction/allowed").and_then(Value::as_array).is_some_and(|items| items.contains(&entry["variable"]));
    }
    let items = if harness == "kimi" { doc["hooks"].as_array() } else { doc["hooks"][event].as_array() };
    items.is_some_and(|items| items.iter().any(|item| {
        item == entry || entry["hooks"].as_array().is_some_and(|expected| {
            item["hooks"].as_array().is_some_and(|actual| expected.iter().all(|h| actual.contains(h)))
        })
    }))
}

fn owned_entries(entries: &BTreeMap<String, Value>) -> BTreeMap<String, Value> {
    entries.iter().map(|(event, entry)| (event.clone(), json!([entry]))).collect()
}

pub fn run(action: &crate::cli::HooksAction) -> Result<()> {
    use crate::cli::HooksAction;
    let home = home()?;
    let dir = config_dir(&home);
    let config = if matches!(action, HooksAction::Install { .. }) { Config::load(&home)? } else { Config::default() };
    let manifest_path = dir.join("hooks-installed.json");
    // Serializes setup commands without leaving a lock on read-only/dry-run paths.
    let dry = matches!(action, HooksAction::Install { dry_run: true, .. });
    let status = matches!(action, HooksAction::Status);
    let _lock = if !dry && !status {
        std::fs::create_dir_all(&dir)?;
        let file = std::fs::OpenOptions::new().create(true).write(true).truncate(false).open(dir.join("hooks.lock"))?;
        file.try_lock().context("another hooks setup command is running")?;
        Some(file)
    } else { None };
    let raw_manifest = read(&manifest_path)?;
    let mut manifest: Manifest = if raw_manifest.is_empty() { BTreeMap::new() } else { serde_json::from_str(&raw_manifest)? };
    for events in manifest.values() {
        for values in events.values() {
            ensure!(values.as_array().is_some_and(|items| !items.is_empty() && items.iter().all(Value::is_object)), "invalid ownership manifest");
        }
    }
    if status {
        println!("config: {}", dir.join("hooks.toml").display());
        if let Err(e) = Config::load(&home) { eprintln!("runtime config invalid: {e}"); }
    }
    let selection = match action { HooksAction::Install { harness, .. } | HooksAction::Uninstall { harness } => harness.as_str(), _ => "all" };
    let names = if selection == "all" { HARNESSES.to_vec() } else { selection.split(',').collect() };
    let binary = std::env::current_exe()?;
    for name in names {
        if !HARNESSES.contains(&name) { println!("skip {name}: no supported command-hook adapter"); continue; }
        let path = target(&home, name);
        if !path.parent().unwrap().is_dir() { println!("skip {name}: profile directory missing"); continue; }
        let raw = read(&path)?;
        let old = manifest.get(name).cloned().unwrap_or_default();
        let mut new: BTreeMap<String, Value> = BTreeMap::new();
        if matches!(action, HooksAction::Install { .. }) {
            for event in EVENTS {
                if config.enabled(name, event) { new.insert(native(name, event).into(), entry(&binary, name, event)); }
            }
        }
        let is_toml = path.extension().is_some_and(|s| s == "toml");
        let doc = parsed(&raw, is_toml)?;
        if name == "gemini" && !new.is_empty() {
            let allowance = json!({"variable": config.token_env});
            let tracked = old.get(GEMINI_ENV).and_then(Value::as_array).is_some_and(|items| items.contains(&allowance));
            if tracked || !present(&doc, name, GEMINI_ENV, &allowance) { new.insert(GEMINI_ENV.into(), allowance); }
        }
        if status {
            let entries: Vec<_> = old.iter().flat_map(|(event, values)| values.as_array().into_iter().flatten().map(move |v| (event, v))).collect();
            let found = entries.iter().filter(|(event, entry)| present(&doc, name, event, entry)).count();
            let state = if entries.is_empty() { "not installed" } else if found == entries.len() { "installed (review harness trust/settings)" } else { "modified or missing" };
            println!("{name}: {state}, {found}/{} entries ({})", entries.len(), path.display());
            continue;
        }
        for (event, entry) in &new {
            let tracked = old.get(event).and_then(Value::as_array).is_some_and(|items| items.contains(entry));
            ensure!(tracked || !present(&doc, name, event, entry), "{name}: identical unowned hook already exists; refusing to claim it");
        }
        let next = if path.extension().is_some_and(|s| s == "toml") { toml_merge(&raw, name, &old, &new)? } else { json_merge(&raw, name, &old, &new)? };
        if dry {
            println!("dry-run {name}: {}\n{}", path.display(), serde_json::to_string_pretty(&new)?);
            continue;
        }
        // Persist ownership first so an interrupted config write can be safely retried/uninstalled.
        // Retain the previous entries until the target update succeeds.
        if !new.is_empty() {
            let mut pending = old.clone();
            for (event, value) in &new {
                let entries = pending.entry(event.clone()).or_insert(json!([])).as_array_mut().context("invalid ownership manifest")?;
                if !entries.contains(value) { entries.push(value.clone()); }
            }
            manifest.insert(name.into(), pending);
            save(&manifest_path, &(serde_json::to_string_pretty(&manifest)? + "\n"))?;
        }
        ensure!(read(&path)? == raw, "{} changed during setup; retry", path.display());
        save(&path, &next).with_context(|| format!("updating {}", path.display()))?;
        if new.is_empty() { manifest.remove(name); }
        else { manifest.insert(name.into(), owned_entries(&new)); }
        save(&manifest_path, &(serde_json::to_string_pretty(&manifest)? + "\n"))?;
        println!("{name}: {} {}", if new.is_empty() { "removed" } else { "installed" }, path.display());
    }
    if !dry && !status {
        save(&manifest_path, &(serde_json::to_string_pretty(&manifest)? + "\n"))?;
        if matches!(action, HooksAction::Install { .. }) && !dir.join("hooks.toml").exists() { save(&dir.join("hooks.toml"), DEFAULT_CONFIG)?; }
    }
    Ok(())
}
