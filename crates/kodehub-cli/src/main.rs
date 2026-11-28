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
    /// List work-item proposals (code changes in flight).
    Proposals {
        /// Only open ones.
        #[arg(long)]
        open: bool,
    },
    /// Project versions (the releases): new | list | freeze KEY | diff A B
    Version {
        #[command(subcommand)]
        cmd: VersionCmd,
    },
    /// Deploy a project version into an environment (production waits for a human approval
    /// in the app).
    Deploy {
        project_version: String,
        env: String,
    },
    /// Roll an environment back to the project version it ran before.
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
        /// Work item whose proposal write tools act on (agent sessions).
        #[arg(long)]
        work_item: Option<String>,
    },
}

#[derive(Subcommand)]
enum VersionCmd {
    /// Open a new project version from the current one and make it current.
    New {
        #[arg(long)]
        name: Option<String>,
    },
    List,
    /// Freeze a project version (it never changes again).
    Freeze {
        key: String,
        #[arg(long)]
        reason: Option<String>,
    },
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
            work_item,
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
                    work_item,
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
                for g in v["tags"].as_array().cloned().unwrap_or_default() {
                    println!(
                        "{} #{} ({} requirements)",
                        icon(g["status"].as_str().unwrap_or("")),
                        g["tag"].as_str().unwrap_or(""),
                        g["members"].as_array().map(|a| a.len()).unwrap_or(0)
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
        Cmd::Proposals { open } => pretty(&c.tool("list_proposals", &json!({"open": open})).await?),
        Cmd::Version { cmd } => match cmd {
            VersionCmd::New { name } => pretty(
                &c.tool("new_project_version", &json!({"name": name}))
                    .await?,
            ),
            VersionCmd::List => pretty(&c.tool("list_project_versions", &json!({})).await?),
            VersionCmd::Freeze { key, reason } => pretty(
                &c.tool(
                    "freeze_project_version",
                    &json!({"project_version": key, "reason": reason}),
                )
                .await?,
            ),
            VersionCmd::Diff { from, to } => pretty(
                &c.call(
                    "GET",
                    &format!("/api/project-versions/{from}/diff/{to}"),
                    None,
                )
                .await?,
            ),
        },
        Cmd::Deploy {
            project_version,
            env,
        } => pretty(
            &c.tool(
                "deploy",
                &json!({"project_version": project_version, "environment": env}),
            )
            .await?,
        ),
        Cmd::Rollback { env } => pretty(&c.tool("rollback", &json!({"environment": env})).await?),
        Cmd::Tool { name, args } => {
            let a: Value = serde_json::from_str(&args)?;
            pretty(&c.tool(&name, &a).await?)
        }
    }
    Ok(())
}
