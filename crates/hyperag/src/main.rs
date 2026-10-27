use std::sync::Arc;

use clap::Parser;
use hyperag::cli::{Cli, Command, ModelsAction, open, open_service, parse_assignments, switch};
use hyperag::mcp::Mcp;
use hyperag::server::{App, TokenFile, models_view, router};
use rmcp::ServiceExt;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService, session::local::LocalSessionManager};

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).with_writer(std::io::stderr).init();
    let command = Cli::parse().command;
    // Hooks do no model work: avoid constructing a thread pool for every prompt.
    let mut builder = if matches!(&command, Command::Hook { .. } | Command::HookWriter { .. } | Command::HookWriterStatus) {
        tokio::runtime::Builder::new_current_thread()
    } else { tokio::runtime::Builder::new_multi_thread() };
    builder.enable_all().build()?.block_on(run(command))
}

async fn run(command: Command) -> anyhow::Result<()> {
    match command {
        Command::Serve { host, port } => {
            let file = std::env::var("HYPERAG_TOKENS_FILE").map_err(|_| anyhow::anyhow!("HYPERAG_TOKENS_FILE is required"))?;
            let tokens = TokenFile::open(file.into())?;
            let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;
            let local = listener.local_addr()?;
            let service = open_service().await?;
            let app = App { hyperag: service.hyperag.clone(), tokens: Arc::new(tokens) };
            // MCP over HTTP at /mcp: each tool call goes back through this service's own routes with
            // the request's bearer token, so it gets the same authentication and scope checks.
            // Call back on the bound address; a wildcard bind is reached on loopback.
            let back = if local.ip().is_unspecified() { std::net::SocketAddr::new([127, 0, 0, 1].into(), local.port()) } else { local };
            let base = format!("http://{back}");
            // rmcp checks the Host header against this list (DNS rebinding); a remote deployment
            // names its own host names in HYPERAG_MCP_ALLOWED_HOSTS.
            let mut hosts = vec!["localhost".to_owned(), "127.0.0.1".to_owned(), "::1".to_owned(), back.ip().to_string()];
            hosts.extend(std::env::var("HYPERAG_MCP_ALLOWED_HOSTS").unwrap_or_default().split(',').filter(|h| !h.is_empty()).map(str::to_owned));
            let mcp_config = StreamableHttpServerConfig::default().with_allowed_hosts(hosts);
            let mcp_cancel = mcp_config.cancellation_token.clone();
            let shutdown = mcp_cancel.clone();
            let mcp = StreamableHttpService::new(
                move || Ok(Mcp::new(&base, None)),
                LocalSessionManager::default().into(),
                mcp_config,
            );
            eprintln!("hyperag listening on http://{local} (MCP at /mcp)");
            let result = axum::serve(listener, router(app).nest_service("/mcp", mcp))
                .with_graceful_shutdown(async move { shutdown_signal().await; shutdown.cancel(); }).await;
            mcp_cancel.cancel();
            let cleanup = service.close().await;
            result?;
            cleanup?;
        }
        Command::Hook { event, harness } => std::process::exit(hyperag::hooks::run_for(&event, harness.as_deref()).await),
        Command::HookWriter { root } => {
            #[cfg(unix)]
            if hyperag::hook_writer::run(&root).await.is_err() { std::process::exit(1); }
            #[cfg(not(unix))]
            { let _ = root; anyhow::bail!("asynchronous hooks require Unix"); }
        }
        Command::HookWriterStatus => {
            #[cfg(unix)]
            {
                let config = hyperag::hook_setup::Config::load(&hyperag::hook_setup::home()?)?;
                let token = config.token()?;
                let base = std::env::var("HYPERAG_URL").unwrap_or(config.url);
                let mut status = hyperag::hook_writer::inspect(&base, &token)?;
                status["async_writes"] = serde_json::json!(config.async_writes);
                status["token_file"] = serde_json::json!(config.token_file.is_some());
                status["async_active_marker"] = serde_json::json!(config.async_active_marker);
                println!("{status}");
            }
            #[cfg(not(unix))]
            anyhow::bail!("asynchronous hooks require Unix");
        }
        Command::Hooks { action } => hyperag::hook_setup::run(&action)?,
        Command::Token { principal, file } => {
            let mut bytes = [0u8; 32];
            getrandom::fill(&mut bytes).map_err(|_| anyhow::anyhow!("secure OS randomness unavailable"))?;
            let token = format!("hr_{}", hex::encode(bytes));
            let mut tokens: serde_json::Map<String, serde_json::Value> = match std::fs::read_to_string(&file) {
                Ok(raw) => serde_json::from_str(&raw)?,
                Err(_) => serde_json::Map::new(),
            };
            tokens.insert(hyperag::server::token_hash(&token), principal.clone().into());
            if let Some(dir) = file.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(&file, serde_json::to_string_pretty(&tokens)?)?;
            eprintln!("added {principal} to {}; the token is shown once:", file.display());
            println!("{token}");
        }
        Command::Mcp { url } => {
            let token = std::env::var("HYPERAG_TOKEN").map_err(|_| anyhow::anyhow!("HYPERAG_TOKEN is required"))?;
            let service = Mcp::new(&url, Some(token)).serve(rmcp::transport::stdio()).await?;
            service.waiting().await?;
        }
        Command::Export { out } => {
            let h = open().await?;
            let counts = if out.as_os_str() == "-" {
                hyperag::portable::export(&h, &mut std::io::BufWriter::new(std::io::stdout().lock())).await?
            } else {
                // Unique sibling staging avoids collisions and preserves the target on failure.
                let parent = out.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or_else(|| std::path::Path::new("."));
                let mut part = tempfile::NamedTempFile::new_in(parent)?;
                let counts = hyperag::portable::export(&h, &mut std::io::BufWriter::new(part.as_file_mut())).await?;
                part.persist(&out)?;
                counts
            };
            eprintln!("exported {}", serde_json::to_string(&counts)?);
        }
        Command::Import { file, embedding, reranker, actor, without_graph } => {
            let records = hyperag::portable::read(std::io::BufReader::new(std::fs::File::open(&file)?))?;
            let h = open().await?;
            let done = hyperag::portable::import(&h, &records, without_graph).await?;
            eprintln!("imported {}", serde_json::to_string(&done)?);
            if embedding.is_some() || reranker.is_some() {
                let actor = actor.ok_or_else(|| anyhow::anyhow!("--as <admin> is required to choose models"))?;
                // The switch embeds every current memory into the new space before it takes effect.
                switch(&h, &actor, embedding.as_deref(), reranker.as_deref()).await?;
            }
            if let Some((space, n)) = h.reindex_active().await? {
                eprintln!("embedded {n} memories into space {space}");
            }
            let (e, space, r) = h.active_models().await;
            eprintln!("models: embedding {} (space {}), reranker {}", e.as_deref().unwrap_or("none"), space.as_deref().unwrap_or("-"), r.as_deref().unwrap_or("none"));
        }
        Command::Models { action: ModelsAction::List { available } } => {
            let h = open().await?;
            println!("{}", serde_json::to_string_pretty(&models_view(&h).await?)?);
            if available {
                for (spec, dims) in hyperag::models::available() {
                    println!("embedding  {spec}  ({dims} dimensions)");
                }
                for spec in hyperag::models::available_rerankers() {
                    println!("reranker   {spec}");
                }
            }
        }
        Command::Models { action: ModelsAction::Pull { embedding, reranker } } => {
            // The active specs: flags, then the environment, then what `models set` stored, then the defaults.
            let stored = match std::env::var("HYPERAG_PG_URL") {
                Ok(url) if !url.is_empty() => {
                    let store = hyperag::store::Store::new(sqlx::postgres::PgPoolOptions::new().max_connections(1).connect(&url).await?);
                    (store.setting("embedding").await.ok().flatten(), store.setting("reranker").await.ok().flatten())
                }
                _ => (None, None),
            };
            let env = |k: &str| std::env::var(k).ok().filter(|v| !v.is_empty());
            let e = embedding.or_else(|| env("HYPERAG_EMBEDDING")).or(stored.0).unwrap_or_else(|| hyperag::cli::DEFAULT_EMBEDDING.into());
            let r = reranker.or_else(|| env("HYPERAG_RERANKER")).or(stored.1).unwrap_or_else(|| hyperag::cli::DEFAULT_RERANKER.into());
            for (slot, spec) in [("embedding", e), ("reranker", r)] {
                let direct = spec.strip_prefix("hf:").and_then(|s| s.split_once('?'));
                match direct {
                    Some((repo, rest)) if slot == "reranker" || hyperag::models::is_direct_hf(&spec) => {
                        let p = |k: &str| rest.split('&').find_map(|kv| kv.strip_prefix(&format!("{k}=")).map(str::to_owned));
                        let files = hyperag::models::hf_files(repo, &p("revision").unwrap_or_else(|| "main".into()), &p("dtype").unwrap_or_else(|| "fp32".into())).await?;
                        let check = if files.unchecked.is_empty() { "SHA-256 checked against the hub".to_owned() } else { format!("not checked: {} (no published SHA-256, or cached before checks were recorded; run online once)", files.unchecked.join(", ")) };
                        println!("{slot}: {repo}, {:.0} MB of weights, {check}, licence {}", files.weight_bytes as f64 / 1e6, files.licence);
                        hyperag::models::record_licence(repo, &files.licence)?;
                    }
                    _ if spec.starts_with("hf:") => println!("{slot}: {spec} is a fastembed preset: fastembed downloads it into the same cache on first load; Hyperag does not check its hash"),
                    _ => println!("{slot}: {spec} (nothing to download)"),
                }
                // Loading proves the model runs here; a preset is downloaded by this step.
                if slot == "embedding" {
                    hyperag::models::Embedder::from_spec(&spec).await?;
                } else {
                    hyperag::models::Reranker::from_spec(&spec).await?;
                }
            }
            println!("Models are in {}; the ones listed as checked load offline from now on.", hyperag::models::cache_root().display());
        }
        Command::Models { action: ModelsAction::Recommend { json } } => {
            let r = hyperag::recommend::detect();
            let picks = hyperag::recommend::recommend(&r);
            if json {
                println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "resources": r, "choices": picks }))?);
            } else {
                println!("Free now: {:.1} GB of {:.1} GB, swap used {:.1} GB, {} cores{}{}", r.free_mb / 1024.0, r.total_mb / 1024.0, r.swap_used_mb / 1024.0, r.cores,
                    if r.apple_silicon { ", Apple silicon" } else { "" },
                    r.gpu_free_mb.map(|g| format!(", GPU {:.1} GB free", g / 1024.0)).unwrap_or_default());
                println!("Memory per model = parameters x bytes per parameter x 1.2 + batch working set.");
                for c in &picks {
                    println!("{:<9} {:<46} {:>7.0} MB  {:<4} {:<26} {}", c.slot, c.model, c.estimate_mb, c.runs_on, c.licence, c.why);
                }
                println!("Every slot is your choice: hyperag models set embedding=<spec> reranker=<spec> --as <admin>");
            }
        }
        Command::Models { action: ModelsAction::Set { assignments, actor } } => {
            let (e, r) = parse_assignments(&assignments)?;
            let h = open().await?;
            switch(&h, &actor, e.as_deref(), r.as_deref()).await?;
            println!("{}", serde_json::to_string_pretty(&models_view(&h).await?)?);
        }
    }
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("install SIGTERM handler");
        tokio::select! { _ = tokio::signal::ctrl_c() => (), _ = terminate.recv() => () }
    }
    #[cfg(not(unix))]
    tokio::signal::ctrl_c().await.expect("install Ctrl-C handler");
}
