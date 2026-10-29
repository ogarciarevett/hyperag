//! `hyperag hook <event>` for agent harness hooks (Claude Code, Codex). It reads the hook's JSON on
//! stdin, talks to the service with the session's token (`HYPERAG_URL`, `HYPERAG_TOKEN`), and never
//! fails the session: any error goes to stderr and the hook exits 0.
//!
//! - `session-start`: prints the newest memories of the project's scope, for the harness to add
//!   to the session's context.
//! - `prompt`: stores the user's prompt (kind `prompt`) in the project's scope.
//! - `session-end`: stores a marker that the session ended (kind `session`).
//!
//! The scope is `HYPERAG_SCOPE`, else `project:<name>` where name is the common git repository
//! folder (so linked worktrees share a scope), or the current folder, lowercased.

use std::path::Path;

use serde_json::{Value, json};

const PROMPT_MAX_CHARS: usize = 4_000;

#[derive(serde::Deserialize)]
struct ScopeGrants {
    read: Vec<String>,
    write: Vec<String>,
}

pub fn scope_for(cwd: &Path) -> String {
    scope_with_override(cwd, std::env::var("HYPERAG_SCOPE").ok().as_deref())
}

fn scope_with_override(cwd: &Path, override_scope: Option<&str>) -> String {
    if let Some(scope) = override_scope.filter(|scope| !scope.is_empty()) {
        return scope.to_owned();
    }
    let root = cwd
        .ancestors()
        .find(|d| d.join(".git").exists())
        .unwrap_or(cwd);
    let common = common_repository(root);
    let root = common.as_deref().unwrap_or(root);
    let name = root
        .file_name()
        .map(|n| n.to_string_lossy().to_lowercase())
        .unwrap_or_else(|| "unknown".into());
    format!("project:{name}")
}

fn common_repository(root: &Path) -> Option<std::path::PathBuf> {
    let marker = root.join(".git");
    if !marker.is_file() {
        return None;
    }
    let metadata = std::fs::read_to_string(marker).ok()?;
    let gitdir = root
        .join(metadata.trim().strip_prefix("gitdir:")?.trim())
        .canonicalize()
        .ok()?;
    let common = match std::fs::read_to_string(gitdir.join("commondir")) {
        Ok(relative) => gitdir.join(relative.trim()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => gitdir,
        Err(_) => return None,
    }
    .canonicalize()
    .ok()?;
    if common.file_name().is_some_and(|name| name == ".git") {
        common.parent().map(Path::to_path_buf)
    } else {
        None
    }
}

pub async fn run(event: &str) -> i32 {
    run_for(event, None).await
}

pub async fn run_for(event: &str, harness: Option<&str>) -> i32 {
    use std::io::Write;
    let started = std::time::Instant::now();
    let launch_age = std::env::var("HYPERAG_HOOK_STARTED_AT")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .map(|epoch| {
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs_f64()
                - epoch
        })
        .filter(|age| age.is_finite() && (0.0..=5.0).contains(age))
        .unwrap_or(0.0);
    let mut report =
        json!({"category": "completed", "steps_ms": {"process_start": launch_age * 1000.0}});
    let report_pipe = report_pipe();
    let result: anyhow::Result<String> = async {
        if let Err(error) = &report_pipe {
            anyhow::bail!("hook report unavailable: {error}");
        }
        let config = crate::hook_setup::Config::load(&crate::hook_setup::home()?)?;
        if let Some(name) = harness {
            anyhow::ensure!(
                crate::hook_setup::HARNESSES.contains(&name),
                "unknown harness"
            );
        }
        anyhow::ensure!(
            crate::hook_setup::EVENTS.contains(&event),
            "unknown common hook event"
        );
        if !config.enabled(harness.unwrap_or(""), event) {
            report["category"] = json!("disabled");
            return Ok(String::new());
        }
        // Leave 10 ms for reporting, then 30 ms for the guard tail within the 250 ms budget.
        let remaining = (0.210 - launch_age - started.elapsed().as_secs_f64()).max(0.0);
        let budget_limited = event == "prompt"
            && std::env::var_os("HYPERAG_HOOK_STARTED_AT").is_some()
            && remaining <= config.timeout_ms as f64 / 1000.0;
        let budget = if budget_limited {
            std::time::Duration::from_secs_f64(remaining)
        } else {
            std::time::Duration::from_millis(config.timeout_ms)
        };
        match tokio::time::timeout(
            budget,
            handle(event, harness, &config, &mut report, started),
        )
        .await
        {
            Ok(result) => result,
            Err(_) if budget_limited => {
                report["category"] = json!("budget_skip");
                Ok(String::new())
            }
            Err(_) => anyhow::bail!("hook timed out"),
        }
    }
    .await;
    let out = match result {
        Ok(text) => text,
        Err(e) => {
            report["category"] = json!("error");
            let _ = writeln!(std::io::stderr(), "hyperag hook {event}: {e}");
            String::new()
        }
    };
    if let Some(step_started) = report
        .as_object_mut()
        .unwrap()
        .remove("_step_started_ms")
        .and_then(|value| value.as_f64())
    {
        let stage = report["stage"].as_str().unwrap_or("setup").to_owned();
        if report["steps_ms"][&stage].is_null() {
            report["steps_ms"][&stage] =
                json!((started.elapsed().as_secs_f64() * 1000.0 - step_started).max(0.0));
        }
    }
    report["steps_ms"]["native_total"] = json!(started.elapsed().as_secs_f64() * 1000.0);
    if let Ok(Some(mut pipe)) = report_pipe {
        if writeln!(pipe, "{report}").is_err() {
            let _ = writeln!(std::io::stderr(), "hyperag hook report failed");
        }
    }
    let output = match harness {
        Some("cursor") if event == "session-start" && !out.is_empty() => json!({"additional_context": out}).to_string(),
        Some("gemini") if event == "session-start" && !out.is_empty() => json!({"hookSpecificOutput": {"hookEventName": "SessionStart", "additionalContext": out}}).to_string(),
        Some("cursor") if event == "prompt" => json!({"continue": true}).to_string(),
        Some("cursor" | "gemini") => "{}".into(),
        _ => out,
    };
    if !output.is_empty() {
        let _ = writeln!(std::io::stdout(), "{output}");
    }
    0
}

// Optional private pipe keeps category-only supervision out of harness stdout/stderr.
fn report_pipe() -> anyhow::Result<Option<std::fs::File>> {
    let Some(value) = std::env::var_os("HYPERAG_HOOK_REPORT_FD") else {
        return Ok(None);
    };
    #[cfg(unix)]
    {
        use std::os::{
            fd::FromRawFd,
            unix::fs::{FileTypeExt, MetadataExt},
        };
        let descriptor: i32 = value
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("invalid report descriptor"))?
            .parse()?;
        anyhow::ensure!(descriptor >= 3, "invalid report descriptor");
        // Validate before adopting the explicitly transferred descriptor.
        let metadata = std::fs::metadata(format!("/dev/fd/{descriptor}"))?;
        anyhow::ensure!(
            metadata.file_type().is_fifo() && metadata.uid() == crate::hook_credentials::owner(),
            "invalid report pipe"
        );
        // Cloning sets close-on-exec; dropping the original prevents writer inheritance.
        let original = unsafe { std::fs::File::from_raw_fd(descriptor) };
        let metadata = original.metadata()?;
        anyhow::ensure!(
            metadata.file_type().is_fifo() && metadata.uid() == crate::hook_credentials::owner(),
            "invalid report pipe"
        );
        return Ok(Some(original.try_clone()?));
    }
    #[cfg(not(unix))]
    {
        let _ = value;
        anyhow::bail!("hook reporting requires Unix")
    }
}

async fn handle(
    event: &str,
    harness: Option<&str>,
    config: &crate::hook_setup::Config,
    report: &mut Value,
    started: std::time::Instant,
) -> anyhow::Result<String> {
    use tokio::io::AsyncReadExt;
    let mut raw = String::new();
    // Bounded input and an outer deadline also protect malformed/incomplete stdin.
    tokio::io::stdin()
        .take(1_048_577)
        .read_to_string(&mut raw)
        .await?;
    anyhow::ensure!(raw.len() <= 1_048_576, "hook input exceeds 1 MiB");
    let input: Value = if raw.trim().is_empty() {
        json!({})
    } else {
        serde_json::from_str(&raw)?
    };
    let cwd_hint = input["cwd"].as_str().or_else(|| {
        if harness == Some("cursor") {
            input["workspace_roots"][0].as_str()
        } else {
            None
        }
    });
    let cwd = match cwd_hint {
        Some(c) => std::path::PathBuf::from(c),
        None => std::env::current_dir()?,
    };
    let scope = std::env::var("HYPERAG_SCOPE")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| {
            if config.scope == "project" {
                scope_for(&cwd)
            } else {
                config.scope.clone()
            }
        });
    anyhow::ensure!(scope.len() <= 1024, "hook scope too long");
    report["scope"] = json!(scope);
    report["stage"] = json!("setup");
    let session = input["session_id"]
        .as_str()
        .or_else(|| input["conversation_id"].as_str())
        .unwrap_or("unknown")
        .to_owned();
    let base = std::env::var("HYPERAG_URL")
        .unwrap_or_else(|_| config.url.clone())
        .trim_end_matches('/')
        .to_owned();
    let token = config.token()?;
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(config.timeout_ms))
        .build()?;
    let token = token.as_str();
    let base = base.as_str();
    let scope = scope.as_str();
    let send = |req: reqwest::RequestBuilder| async move {
        let response = req.bearer_auth(token).send().await?;
        let status = response.status();
        let body: Value = response.json().await.unwrap_or(Value::Null);
        anyhow::ensure!(status.is_success(), "service answered {status}");
        Ok::<Value, anyhow::Error>(body)
    };
    report["steps_ms"]["input_setup"] = json!(started.elapsed().as_secs_f64() * 1000.0);
    report["stage"] = json!("scopes_get");
    report["_step_started_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
    let step = std::time::Instant::now();
    let response = send(http.get(format!("{base}/v1/scopes"))).await;
    report["steps_ms"]["scopes_get"] = json!(step.elapsed().as_secs_f64() * 1000.0);
    let grants = response?;
    let grants: ScopeGrants = serde_json::from_value(grants)
        .map_err(|_| anyhow::anyhow!("invalid scope grant response"))?;
    if !grants.read.iter().any(|value| value == scope)
        || !grants.write.iter().any(|value| value == scope)
    {
        #[cfg(unix)]
        if config.async_writes {
            crate::hook_writer::skipped_scope(base, token).await?;
        }
        report["category"] = json!("skipped_ungranted");
        return Ok(String::new());
    }
    match event {
        "session-start" => {
            let url = format!(
                "{base}/v1/recent?scope={}&limit=30&maxBytes=12000",
                crate::mcp::urlencode(scope)
            );
            report["stage"] = json!("recent_get");
            report["_step_started_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
            let step = std::time::Instant::now();
            let response = send(http.get(url)).await;
            report["steps_ms"]["recent_get"] = json!(step.elapsed().as_secs_f64() * 1000.0);
            let body = response?;
            let text = body["text"].as_str().unwrap_or_default();
            Ok(if text.is_empty() {
                String::new()
            } else {
                format!("Hyperag memory, newest first (scope {scope}):\n{text}")
            })
        }
        "prompt" => {
            let prompt: String = input["prompt"]
                .as_str()
                .unwrap_or_default()
                .chars()
                .take(PROMPT_MAX_CHARS)
                .collect();
            if prompt.trim().is_empty() {
                report["category"] = json!("empty_prompt");
                return Ok(String::new());
            }
            let body = json!({ "scope": scope, "kind": "prompt", "source": format!("session {session}"), "text": prompt });
            if config.async_writes {
                report["stage"] = json!("writer_enqueue");
                report["_step_started_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
                let step = std::time::Instant::now();
                let result = enqueue_write(base, token, body, config).await;
                report["steps_ms"]["writer_enqueue"] = json!(step.elapsed().as_secs_f64() * 1000.0);
                let accepted = result?;
                report["category"] = json!(if accepted { "enqueued" } else { "cancelled" });
                return Ok(String::new());
            }
            send(http.post(format!("{base}/v1/memories")).json(&body)).await?;
            Ok(String::new())
        }
        "session-end" => {
            let reason = input["reason"].as_str().unwrap_or("ended");
            let text = format!("Session {session} ended ({reason}).");
            let body = json!({ "scope": scope, "kind": "session", "source": format!("session {session}"), "text": text });
            if config.async_writes {
                report["stage"] = json!("writer_enqueue");
                report["_step_started_ms"] = json!(started.elapsed().as_secs_f64() * 1000.0);
                let step = std::time::Instant::now();
                let result = enqueue_write(base, token, body, config).await;
                report["steps_ms"]["writer_enqueue"] = json!(step.elapsed().as_secs_f64() * 1000.0);
                let accepted = result?;
                report["category"] = json!(if accepted { "enqueued" } else { "cancelled" });
                return Ok(String::new());
            }
            send(http.post(format!("{base}/v1/memories")).json(&body)).await?;
            Ok(String::new())
        }
        other => {
            anyhow::bail!("unknown hook event {other}; use session-start, prompt or session-end")
        }
    }
}

async fn enqueue_write(
    base: &str,
    token: &str,
    body: Value,
    config: &crate::hook_setup::Config,
) -> anyhow::Result<bool> {
    #[cfg(unix)]
    {
        crate::hook_writer::enqueue(base, token, body, config.async_active_marker.as_deref()).await
    }
    #[cfg(not(unix))]
    {
        let _ = (base, token, body, config);
        anyhow::bail!("asynchronous hooks require Unix")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_from_git_root_or_folder() {
        let dir = std::env::temp_dir().join(format!("hyperag-hook-test-{}", std::process::id()));
        let nested = dir.join("Repo-X").join("src").join("deep");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(dir.join("Repo-X").join(".git")).unwrap();
        assert_eq!(scope_with_override(&nested, None), "project:repo-x");
        assert_eq!(
            scope_with_override(&dir, None),
            format!(
                "project:{}",
                dir.file_name().unwrap().to_string_lossy().to_lowercase()
            )
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn linked_worktree_uses_common_repository_scope_and_explicit_override() {
        let fixture = tempfile::tempdir().unwrap();
        let repository = fixture.path().join("Coordination");
        let worktree = fixture.path().join("chief");
        let git = |args: &[&str]| {
            let output = std::process::Command::new("git")
                .env_remove("GIT_DIR")
                .env_remove("GIT_WORK_TREE")
                .env_remove("GIT_COMMON_DIR")
                .args([
                    "-c",
                    "core.hooksPath=/dev/null",
                    "-c",
                    "commit.gpgSign=false",
                ])
                .args(args)
                .output()
                .unwrap();
            assert!(output.status.success(), "fixture git command failed");
        };
        git(&["init", repository.to_str().unwrap()]);
        git(&[
            "-C",
            repository.to_str().unwrap(),
            "-c",
            "user.name=Fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "--allow-empty",
            "-m",
            "fixture",
        ]);
        git(&[
            "-C",
            repository.to_str().unwrap(),
            "worktree",
            "add",
            "-b",
            "chief",
            worktree.to_str().unwrap(),
        ]);
        let nested = worktree.join("src");
        std::fs::create_dir(&nested).unwrap();
        assert_eq!(
            scope_with_override(&repository, None),
            "project:coordination"
        );
        assert_eq!(scope_with_override(&nested, None), "project:coordination");
        assert_eq!(
            scope_with_override(&nested, Some("")),
            "project:coordination"
        );
        assert_eq!(
            scope_with_override(&nested, Some("project:explicit")),
            "project:explicit"
        );
        std::fs::write(
            worktree.join(".git"),
            "gitdir: ../Coordination/.git/worktrees/chief\n",
        )
        .unwrap();
        assert_eq!(scope_with_override(&nested, None), "project:coordination");
    }

    #[test]
    fn custom_git_directory_falls_back_to_worktree_name() {
        let fixture = tempfile::tempdir().unwrap();
        let worktree = fixture.path().join("Repo-X");
        let metadata = fixture.path().join("custom-metadata");
        std::fs::create_dir(&worktree).unwrap();
        std::fs::create_dir(&metadata).unwrap();
        std::fs::write(worktree.join(".git"), "gitdir: ../custom-metadata\n").unwrap();
        assert_eq!(scope_with_override(&worktree, None), "project:repo-x");
    }
}
