mod agent;
mod auth;
mod config;
mod health;
mod ingest;
mod server;
mod store;

use std::{path::PathBuf, sync::Arc};

use anyhow::{Context, Result, bail};
use rmcp::transport::streamable_http_server::{
    StreamableHttpServerConfig, StreamableHttpService, session::never::NeverSessionManager,
};
use tokio::signal::unix::{SignalKind, signal};

const USAGE: &str = "\
usage:
  docs-mcp serve                                 run the MCP server
  docs-mcp ingest <file> <id> <title> [description]
                                                 add or replace a document (.pdf, .md,
                                                 .txt, .rst, .html), e.g.
                                                 docs-mcp ingest manual.pdf widget-2.0 'Widget Manual 2.0'
  docs-mcp remove <id>                           remove a document
  docs-mcp sections [id]                         print a document's sections
  docs-mcp token <source>                        mint a client token

environment:
  DOCS_DB             database path (default: docs.db)
  DOCS_TOKENS         tokens file (default: tokens)
  DOCS_CONFIG         deployment config, a TOML file (see examples/)
  DOCS_ADDR           listen address (default: 127.0.0.1:8770)
  DOCS_ALLOWED_HOSTS  comma-separated Host headers to accept (default: localhost)
  ANTHROPIC_API_KEY   enables ask_docs and summarize (or ANTHROPIC_AUTH_TOKEN, an OAuth token)";

fn env(k: &str, default: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| default.to_string())
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '.' || c == '_')
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "docs_mcp=info".into()),
        )
        .init();

    let args: Vec<String> = std::env::args().skip(1).collect();
    let open = || store::Store::open(env("DOCS_DB", "docs.db"));
    match args.iter().map(String::as_str).collect::<Vec<_>>()[..] {
        ["serve"] => serve().await,
        ["ingest", file, id, title, ref description @ ..] if description.len() <= 1 => {
            if !valid_id(id) {
                bail!("id must be letters, digits, '-', '_' and '.', e.g. widget-2.0");
            }
            let path = std::path::Path::new(file);
            let format = ingest::Format::of(path)?;
            let doc = ingest::ingest(path)?;
            let numbered = doc.sections.iter().filter(|s| s.number.is_some()).count();
            open()?.replace(
                &store::NewDoc {
                    id,
                    title,
                    description: description.first().copied(),
                    format: match format {
                        ingest::Format::Pdf => "pdf",
                        ingest::Format::Markdown => "markdown",
                        ingest::Format::Html => "html",
                    },
                    source: file,
                },
                &doc,
            )?;
            eprintln!(
                "ingested {id}: {} pages, {} sections ({numbered} numbered)",
                doc.pages.len(),
                doc.sections.len()
            );
            Ok(())
        }
        ["remove", id] => {
            if !open()?.remove(id)? {
                bail!("no document {id:?}");
            }
            eprintln!("removed {id}");
            Ok(())
        }
        ["sections"] | ["sections", _] => {
            let store = open()?;
            print_sections(&store, args.get(1).cloned(), None, 0).await
        }
        ["token", source] => {
            if source.is_empty() || source.contains(char::is_whitespace) {
                bail!("source must be a single word, e.g. claude-code");
            }
            let token = auth::generate();
            eprintln!("Token (give this to the client; it is not stored):\n{token}\n");
            eprintln!("Append this line to the tokens file:");
            println!("{} {source}", auth::hash(&token));
            Ok(())
        }
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(2);
        }
    }
}

async fn print_sections(
    store: &store::Store,
    doc: Option<String>,
    under: Option<String>,
    depth: usize,
) -> Result<()> {
    let (doc, sections) = store.contents(doc, under).await?;
    for s in sections {
        let page = s.page.map(|p| format!(" (p. {p})")).unwrap_or_default();
        println!("{:indent$}{} {}{page}", "", s.id, s.title, indent = depth * 2);
        Box::pin(print_sections(store, Some(doc.clone()), Some(s.id), depth + 1)).await?;
    }
    Ok(())
}

async fn serve() -> Result<()> {
    let db_path = env("DOCS_DB", "docs.db");
    let tokens_path = PathBuf::from(env("DOCS_TOKENS", "tokens"));
    let addr = env("DOCS_ADDR", "127.0.0.1:8770");
    let allowed_hosts: Vec<String> = env("DOCS_ALLOWED_HOSTS", "localhost,127.0.0.1,::1")
        .split(',')
        .map(|h| h.trim().to_string())
        .filter(|h| !h.is_empty())
        .collect();
    let config_path = std::env::var_os("DOCS_CONFIG").map(PathBuf::from);
    let config = config::Config::load(config_path.as_deref())?;

    let store = store::Store::open(&db_path)?;
    let tokens = auth::Tokens::load(&tokens_path)?;
    let auth = match (
        std::env::var("ANTHROPIC_API_KEY"),
        std::env::var("ANTHROPIC_AUTH_TOKEN"),
    ) {
        (Ok(key), _) if !key.is_empty() => Some(agent::Auth::ApiKey(key)),
        (_, Ok(token)) if !token.is_empty() => Some(agent::Auth::Bearer(token)),
        _ => None,
    };
    let agent = match auth {
        Some(auth) => Some(agent::Agent::new(auth, config.clone(), store.clone())?),
        None => {
            tracing::warn!("ANTHROPIC_API_KEY is not set; ask_docs and summarize are disabled");
            None
        }
    };
    let health = health::Health {
        store: store.clone(),
        agent: agent.is_some(),
    };

    let mcp = StreamableHttpService::new(
        move || Ok(server::DocsServer::new(store.clone(), agent.clone(), &config)),
        // No sessions: every request stands alone, so a restart never
        // strands a connected client.
        Arc::new(NeverSessionManager::default()),
        StreamableHttpServerConfig::default()
            .with_allowed_hosts(allowed_hosts)
            .with_legacy_session_mode(false)
            .with_json_response(true),
    );
    let app = axum::Router::new()
        .nest_service("/mcp", mcp)
        .layer(axum::middleware::from_fn_with_state(
            tokens.clone(),
            auth::middleware,
        ))
        // Unauthenticated, for an uptime monitor; outside the auth layer.
        .route(
            "/health",
            axum::routing::get(health::handler).with_state(health),
        );

    // `systemctl reload docs-mcp` re-reads the tokens file without a restart.
    let mut hangup = signal(SignalKind::hangup())?;
    tokio::spawn(async move {
        while hangup.recv().await.is_some() {
            match tokens.reload() {
                Ok(n) => tracing::info!(tokens = n, "reloaded tokens"),
                Err(e) => tracing::error!("reloading tokens (keeping the old ones): {e:#}"),
            }
        }
    });

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!("listening on http://{addr}/mcp (db: {db_path})");
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let mut term = signal(SignalKind::terminate()).expect("installing SIGTERM handler");
            tokio::select! {
                _ = term.recv() => {}
                _ = tokio::signal::ctrl_c() => {}
            }
            tracing::info!("shutting down");
        })
        .await?;
    Ok(())
}
