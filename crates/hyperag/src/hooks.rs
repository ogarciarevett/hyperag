//! `hyperag hook <event>` for agent harness hooks (Claude Code, Codex). It reads the hook's JSON on
//! stdin, talks to the service with the session's token (`HYPERAG_URL`, `HYPERAG_TOKEN`), and never
//! fails the session: any error goes to stderr and the hook exits 0.
//!
//! - `session-start`: prints the newest memories of the project's scope, for the harness to add
//!   to the session's context.
//! - `prompt`: stores the user's prompt (kind `prompt`) in the project's scope.
//! - `session-end`: stores a marker that the session ended (kind `session`).
//!
//! The scope is `HYPERAG_SCOPE`, else `project:<name>` where name is the git root folder of the
//! hook's `cwd` (or the folder itself), lowercased.

use std::path::Path;

use serde_json::{Value, json};

const PROMPT_MAX_CHARS: usize = 4_000;

pub fn scope_for(cwd: &Path) -> String {
    if let Ok(s) = std::env::var("HYPERAG_SCOPE") {
        if !s.is_empty() {
            return s;
        }
    }
    let root = cwd.ancestors().find(|d| d.join(".git").exists()).unwrap_or(cwd);
    let name = root.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_else(|| "unknown".into());
    format!("project:{name}")
}

pub async fn run(event: &str) -> i32 {
    // Writes ignore errors (a closed stdout must not panic): the hook always exits 0.
    use std::io::Write;
    match handle(event).await {
        Ok(out) => {
            if !out.is_empty() {
                let _ = writeln!(std::io::stdout(), "{out}");
            }
        }
        Err(e) => {
            let _ = writeln!(std::io::stderr(), "hyperag hook {event}: {e}");
        }
    }
    0
}

async fn handle(event: &str) -> anyhow::Result<String> {
    let mut raw = String::new();
    std::io::Read::read_to_string(&mut std::io::stdin(), &mut raw)?;
    let input: Value = if raw.trim().is_empty() { json!({}) } else { serde_json::from_str(&raw)? };
    let cwd = match input["cwd"].as_str() {
        Some(c) => std::path::PathBuf::from(c),
        None => std::env::current_dir()?,
    };
    let scope = scope_for(&cwd);
    let session = input["session_id"].as_str().unwrap_or("unknown").to_owned();
    let base = std::env::var("HYPERAG_URL").unwrap_or_else(|_| "http://127.0.0.1:7480".into());
    let token = std::env::var("HYPERAG_TOKEN").map_err(|_| anyhow::anyhow!("HYPERAG_TOKEN is not set"))?;
    // Under the harness's hook timeouts (5 s and 10 s), so the hook can log its own error.
    let http = reqwest::Client::builder().timeout(std::time::Duration::from_secs(4)).build()?;
    let send = |req: reqwest::RequestBuilder| async move {
        let r = req.bearer_auth(&token).send().await?;
        let status = r.status();
        let body: Value = r.json().await.unwrap_or(Value::Null);
        anyhow::ensure!(status.is_success(), "service answered {status}: {body}");
        Ok::<Value, anyhow::Error>(body)
    };
    match event {
        "session-start" => {
            let url = format!("{base}/v1/recent?scope={}&limit=30&maxBytes=12000", crate::mcp::urlencode(&scope));
            let body = send(http.get(url)).await?;
            let text = body["text"].as_str().unwrap_or_default();
            Ok(if text.is_empty() { String::new() } else { format!("Hyperag memory, newest first (scope {scope}):\n{text}") })
        }
        "prompt" => {
            let prompt: String = input["prompt"].as_str().unwrap_or_default().chars().take(PROMPT_MAX_CHARS).collect();
            if prompt.trim().is_empty() {
                return Ok(String::new());
            }
            send(http.post(format!("{base}/v1/memories")).json(&json!({ "scope": scope, "kind": "prompt", "source": format!("session {session}"), "text": prompt }))).await?;
            Ok(String::new())
        }
        "session-end" => {
            let reason = input["reason"].as_str().unwrap_or("ended");
            let text = format!("Session {session} ended ({reason}).");
            send(http.post(format!("{base}/v1/memories")).json(&json!({ "scope": scope, "kind": "session", "source": format!("session {session}"), "text": text }))).await?;
            Ok(String::new())
        }
        other => anyhow::bail!("unknown hook event {other}; use session-start, prompt or session-end"),
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
        assert_eq!(scope_for(&nested), "project:repo-x");
        assert_eq!(scope_for(&dir), format!("project:{}", dir.file_name().unwrap().to_string_lossy().to_lowercase()));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
