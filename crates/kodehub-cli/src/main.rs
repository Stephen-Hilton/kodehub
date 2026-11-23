use std::collections::BTreeSet;

use clap::{Parser, Subcommand};
use kodehub_cli::mcp::{serve_stdio, McpOptions};
use kodehub_cli::{server_url, Client};
use serde_json::{json, Value};

#[derive(Parser)]
#[command(
    name = "kodehub",
    about = "Kodehub command-line client (talks to kodehub-server over HTTP)"
)]
struct Cli {
    /// Server URL (default: KODEHUB_SERVER_URL or config).
    #[arg(long, global = true)]
    server: Option<String>,
    /// Print raw JSON.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Server and database health.
    Health,
    /// Import a YAML seed directory (e.g. examples/auth-poc).
    Seed { dir: std::path::PathBuf },
    /// Run a function now: kodehub run verify_credentials --args '{...}' --env dev
    Run {
        symbol: String,
        #[arg(long, default_value = "{}")]
        args: String,
        #[arg(long, default_value = "dev")]
        env: String,
    },
    /// Run tests (all, or for one symbol/requirement).
    Test {
        #[arg(long)]
        symbol: Option<String>,
        #[arg(long)]
        requirement: Option<String>,
        #[arg(long)]
        force: bool,
    },
    /// Coverage of every requirement.
    Coverage { requirement: Option<String> },
    /// Symbols no requirement reaches.
    Orphans,
    /// Requirements at risk if a symbol changes.
    Impact { symbol: String },
    /// Symbols sharing a content hash.
    Duplicates,
    /// List requirements.
    Requirements,
    /// List changesets.
    Changesets,
    /// Releases: cut | list | diff A B
    Release {
        #[command(subcommand)]
        cmd: ReleaseCmd,
    },
    /// Promote a release to an environment (prod needs --approve).
    Promote {
        release: String,
        env: String,
        #[arg(long)]
        approve: bool,
    },
    /// Roll an environment back to its previous release.
    Rollback { env: String },
    /// Call any graph API tool: kodehub tool coverage '{}'
    Tool {
        name: String,
        #[arg(default_value = "{}")]
        args: String,
    },
    /// Run the MCP server on stdio (for Claude Code: `claude mcp add kodehub -- kodehub mcp`).
    Mcp {
        /// Expose only read-only tools.
        #[arg(long)]
        read_only: bool,
        /// Comma-separated allowlist of tool names.
        #[arg(long)]
        tools: Option<String>,
        /// Default changeset for write tools.
        #[arg(long)]
        changeset: Option<String>,
    },
}

#[derive(Subcommand)]
enum ReleaseCmd {
    Cut {
        #[arg(long)]
        name: Option<String>,
        #[arg(long, default_value = "")]
        notes: String,
    },
    List,
    Diff {
        from: String,
        to: String,
    },
}

fn icon(status: &str) -> &'static str {
    match status {
        "pass" => "✔",
        "fail" => "✘",
        "at_risk" | "not_run" => "◐",
        _ => "○",
    }
}

#[tokio::main]
async fn main() {
    if let Err(e) = real_main().await {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

async fn real_main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let c = Client::new(&server_url(cli.server.clone()));
    let pretty = |v: &Value| println!("{}", serde_json::to_string_pretty(v).unwrap());
    match cli.cmd {
        Cmd::Mcp {
            read_only,
            tools,
            changeset,
        } => {
            let allow = tools.map(|t| {
                t.split(',')
                    .map(|s| s.trim().to_string())
                    .collect::<BTreeSet<_>>()
            });
            serve_stdio(
                c,
                McpOptions {
                    read_only,
                    allow,
                    changeset,
                },
            )
            .await?;
        }
        Cmd::Health => pretty(&c.call("GET", "/health", None).await?),
        Cmd::Seed { dir } => {
            let p = std::fs::canonicalize(&dir)?;
            let v = c
                .call("POST", "/api/seed", Some(&json!({"path": p})))
                .await?;
            if cli.json {
                pretty(&v)
            } else {
                println!("seeded {}: {} symbols created, {} changed, {} new versions, {} documents, {} edges",
                    dir.display(), v["symbols_created"], v["symbols_changed"], v["versions_created"],
                    v["documents_created"], v["edges_created"]);
            }
        }
        Cmd::Run { symbol, args, env } => {
            let args: Value = serde_json::from_str(&args)
                .map_err(|e| anyhow::anyhow!("--args must be JSON: {e}"))?;
            let v = c
                .call(
                    "POST",
                    "/api/run",
                    Some(&json!({"symbol": symbol, "args": args, "environment": env})),
                )
                .await?;
            if cli.json {
                pretty(&v)
            } else {
                if !v["stdout"].as_str().unwrap_or("").is_empty() {
                    println!("{}", v["stdout"].as_str().unwrap());
                }
                if !v["stderr"].as_str().unwrap_or("").is_empty() {
                    eprintln!("{}", v["stderr"].as_str().unwrap());
                }
                println!("{}", serde_json::to_string_pretty(&v["result"]).unwrap());
                eprintln!(
                    "{} {} in {}ms ({}){}",
                    if v["ok"] == true { "✔" } else { "✘" },
                    v["display_name"].as_str().unwrap_or(""),
                    v["duration_ms"],
                    if v["cache_hit"] == true {
                        "cache hit"
                    } else {
                        "compiled"
                    },
                    if v["timed_out"] == true {
                        " — TIMED OUT"
                    } else {
                        ""
                    }
                );
                if v["ok"] != true {
                    std::process::exit(2);
                }
            }
        }
        Cmd::Test {
            symbol,
            requirement,
            force,
        } => {
            let scope = match (symbol, requirement) {
                (Some(s), _) => json!({"kind": "symbol", "id": s}),
                (_, Some(r)) => json!({"kind": "requirement", "id": r}),
                _ => json!({"kind": "all"}),
            };
            let v = c
                .call(
                    "POST",
                    "/api/tests/run",
                    Some(&json!({"scope": scope, "force": force})),
                )
                .await?;
            if cli.json {
                pretty(&v)
            } else {
                for r in v["results"].as_array().cloned().unwrap_or_default() {
                    println!(
                        "{} {:<40} {}{}",
                        icon(r["status"].as_str().unwrap_or("")),
                        r["name"].as_str().unwrap_or(""),
                        r["language"].as_str().unwrap_or(""),
                        if r["cached"] == true { " (cached)" } else { "" }
                    );
                }
                println!(
                    "{} passed, {} failed",
                    v["summary"]["passed"], v["summary"]["failed"]
                );
                if v["summary"]["failed"].as_u64().unwrap_or(0) > 0 {
                    std::process::exit(2);
                }
            }
        }
        Cmd::Coverage { requirement } => {
            let v = c
                .tool("coverage", &json!({"requirement": requirement}))
                .await?;
            if cli.json {
                pretty(&v)
            } else {
                for g in v["groups"].as_array().cloned().unwrap_or_default() {
                    println!(
                        "{} {} [{}]",
                        icon(g["status"].as_str().unwrap_or("")),
                        g["title"].as_str().unwrap_or(""),
                        g["logic"].as_str().unwrap_or("")
                    );
                }
                for r in v["requirements"].as_array().cloned().unwrap_or_default() {
                    println!(
                        "  {} {:<36} {:<9} {} tests",
                        icon(r["status"].as_str().unwrap_or("")),
                        r["title"].as_str().unwrap_or(""),
                        r["status"].as_str().unwrap_or(""),
                        r["tests"].as_array().map(|a| a.len()).unwrap_or(0)
                    );
                }
            }
        }
        Cmd::Orphans => pretty(&c.tool("orphans", &json!({})).await?),
        Cmd::Impact { symbol } => pretty(&c.tool("impact", &json!({"symbol": symbol})).await?),
        Cmd::Duplicates => pretty(&c.tool("duplicates", &json!({})).await?),
        Cmd::Requirements => pretty(&c.tool("list_requirements", &json!({})).await?),
        Cmd::Changesets => pretty(&c.tool("list_changesets", &json!({})).await?),
        Cmd::Release { cmd } => match cmd {
            ReleaseCmd::Cut { name, notes } => pretty(
                &c.tool("cut_release", &json!({"name": name, "notes": notes}))
                    .await?,
            ),
            ReleaseCmd::List => pretty(&c.call("GET", "/api/releases", None).await?),
            ReleaseCmd::Diff { from, to } => pretty(
                &c.call("GET", &format!("/api/releases/{from}/diff/{to}"), None)
                    .await?,
            ),
        },
        Cmd::Promote {
            release,
            env,
            approve,
        } => {
            let body = json!({"release": release, "environment": env, "approved_by": if approve { Some("human") } else { None }});
            pretty(&c.tool("promote", &body).await?)
        }
        Cmd::Rollback { env } => pretty(&c.tool("rollback", &json!({"environment": env})).await?),
        Cmd::Tool { name, args } => {
            let a: Value = serde_json::from_str(&args)?;
            pretty(&c.tool(&name, &a).await?)
        }
    }
    Ok(())
}
