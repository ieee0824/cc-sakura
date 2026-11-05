mod diagnostics;
mod mcp;
mod server;
mod translate;
mod web;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::path::PathBuf;

#[derive(Parser)]
#[command(version, about = "AI agents' shared chat, with human-readable history")]
struct Cli {
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[arg(long, global = true)]
    server: Option<String>,
    #[arg(long, global = true)]
    session: Option<String>,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Serve {
        #[arg(long, default_value = "127.0.0.1:8739")]
        bind: std::net::SocketAddr,
        #[arg(long, default_value = "sakura.sqlite3")]
        db: PathBuf,
    },
    Join {
        room: String,
        handle: String,
        #[arg(long)]
        id: Option<uuid::Uuid>,
    },
    Send {
        room: String,
        /// JSON payload, e.g. '{"task":"tests","status":"ok"}'
        payload: String,
        #[arg(long)]
        codec: Option<String>,
        /// Optional human-readable interpretation
        #[arg(long)]
        gloss: Option<String>,
        #[arg(long)]
        mention: Vec<uuid::Uuid>,
    },
    Codec {
        room: String,
        name: String,
        template: String,
    },
    History {
        room: String,
        #[arg(long, default_value_t = 0)]
        after: i64,
        #[arg(long, default_value_t = 100)]
        limit: u32,
        #[arg(long)]
        mentions: Option<uuid::Uuid>,
        #[arg(long)]
        raw: bool,
        /// Local CLI to translate messages without a codec/gloss (default: auto)
        #[arg(long, value_enum, conflicts_with = "raw")]
        translator: Option<translate::Provider>,
        /// Output language for AI interpretations (default: 日本語)
        #[arg(long, conflicts_with = "raw")]
        language: Option<String>,
        /// Ignore cached AI interpretations
        #[arg(long, conflicts_with = "raw")]
        refresh_translations: bool,
        #[arg(long)]
        follow: bool,
    },
    /// Open a browser-based room timeline (localhost by default)
    Web {
        room: String,
        /// Enable decision buttons; sends as the configured participant
        #[arg(long)]
        allow_approvals: bool,
        /// Interface address; use a specific LAN IP to allow other devices
        #[arg(long, default_value = "127.0.0.1")]
        bind: std::net::IpAddr,
        #[arg(long, default_value_t = 8740)]
        port: u16,
        #[arg(long, value_enum)]
        translator: Option<translate::Provider>,
        #[arg(long)]
        language: Option<String>,
    },
    Mcp,
}
#[derive(Default, Deserialize, Serialize)]
struct Config {
    server: Option<String>,
    session: Option<String>,
    #[serde(default)]
    translation: translate::Settings,
}

#[derive(Clone)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    session: Option<String>,
}
pub struct Events {
    response: reqwest::Response,
    buffer: Vec<u8>,
}
impl Events {
    async fn next(&mut self) -> Result<Option<Value>> {
        loop {
            let boundary = self
                .buffer
                .windows(2)
                .position(|w| w == b"\n\n")
                .map(|end| (end, 2))
                .or_else(|| {
                    self.buffer
                        .windows(4)
                        .position(|w| w == b"\r\n\r\n")
                        .map(|end| (end, 4))
                });
            if let Some((end, separator)) = boundary {
                let frame: Vec<u8> = self.buffer.drain(..end + separator).collect();
                let text = std::str::from_utf8(&frame).context("invalid event UTF-8")?;
                let data = text
                    .lines()
                    .filter_map(|line| line.strip_prefix("data:").map(str::trim_start))
                    .collect::<Vec<_>>()
                    .join("\n");
                if !data.is_empty() {
                    return Ok(Some(serde_json::from_str(&data)?));
                }
                continue;
            }
            match tokio::time::timeout(std::time::Duration::from_secs(45), self.response.chunk())
                .await
                .context("event heartbeat timed out")??
            {
                Some(chunk) => {
                    self.buffer.extend_from_slice(&chunk);
                    if self.buffer.len() > 256 * 1024 * 1024 {
                        bail!("event exceeds 256 MiB");
                    }
                }
                None => return Ok(None),
            }
        }
    }
}
fn transient(error: &anyhow::Error) -> bool {
    error.downcast_ref::<reqwest::Error>().is_some_and(|e| {
        if let Some(status) = e.status() {
            return status.is_server_error();
        }
        e.is_connect() || e.is_request() || e.is_timeout() || e.is_body()
    })
}
impl Client {
    // A dedicated client has no whole-response timeout: SSE is intentionally long-lived.
    async fn events(&self, args: &Value) -> Result<Option<Events>> {
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .build()?;
        let mut req = http
            .get(format!("{}/api/events", self.base))
            .query(args)
            .header("Accept", "text/event-stream");
        if let Some(token) = &self.session {
            req = req.bearer_auth(token);
        }
        let response = tokio::time::timeout(std::time::Duration::from_secs(15), req.send())
            .await
            .context("event connection timed out")??;
        if matches!(response.status().as_u16(), 404 | 405) {
            return Ok(None);
        }
        let response = response.error_for_status()?;
        if !response
            .headers()
            .get("content-type")
            .is_some_and(|v| v.to_str().unwrap_or("").starts_with("text/event-stream"))
        {
            bail!("server did not return an event stream");
        }
        Ok(Some(Events {
            response,
            buffer: Vec::new(),
        }))
    }

    async fn receive(&self, mut args: Value) -> Result<Value> {
        let deadline = tokio::time::Instant::now()
            + std::time::Duration::from_secs(
                args["timeout_seconds"].as_u64().unwrap_or(300).min(300),
            );
        let mut legacy = false;
        let mut delay = 1;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            args["timeout_seconds"] = json!(remaining.as_secs().max(1));
            match self
                .call(if legacy { "history" } else { "receive" }, args.clone())
                .await
            {
                Ok(value) => {
                    if !legacy
                        || value["messages"]
                            .as_array()
                            .is_some_and(|ms| !ms.is_empty())
                        || tokio::time::Instant::now() >= deadline
                    {
                        return Ok(value);
                    }
                    tokio::time::sleep_until(
                        (tokio::time::Instant::now() + std::time::Duration::from_secs(1))
                            .min(deadline),
                    )
                    .await;
                }
                Err(error) if !legacy && error.to_string() == "unknown operation" => legacy = true,
                Err(error) => {
                    if !transient(&error) || tokio::time::Instant::now() >= deadline {
                        return Err(error);
                    }
                    eprintln!("receive reconnect: {error}");
                    tokio::time::sleep_until(
                        (tokio::time::Instant::now() + std::time::Duration::from_secs(delay))
                            .min(deadline),
                    )
                    .await;
                    delay = (delay * 2).min(10);
                }
            }
        }
    }
    async fn call(&self, operation: &str, args: Value) -> Result<Value> {
        let mut req = self
            .http
            .post(format!("{}/api/{operation}", self.base))
            .json(&args);
        if let Some(token) = &self.session {
            req = req.bearer_auth(token);
        }
        if operation == "receive" {
            req = req.timeout(std::time::Duration::from_secs(330));
        }
        let response = req.send().await.context("connect to sakura server")?;
        let status = response.status();
        if status.is_server_error() {
            response.error_for_status_ref()?;
        }
        let value: Value = response.json().await.context("invalid server response")?;
        if !status.is_success() {
            bail!("{}", value["error"].as_str().unwrap_or("request failed"));
        }
        Ok(value)
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let path = cli.config.unwrap_or_else(|| {
        PathBuf::from(std::env::var("HOME").unwrap_or_default()).join(".config/ccSakura.toml")
    });
    let config: Config = match std::fs::read_to_string(&path) {
        Ok(s) => toml::from_str(&s).context("invalid config")?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Config::default(),
        Err(e) => return Err(e.into()),
    };
    let client = Client {
        http: reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?,
        base: cli
            .server
            .or(config.server)
            .unwrap_or_else(|| "http://127.0.0.1:8739".into())
            .trim_end_matches('/')
            .into(),
        session: cli.session.or(config.session),
    };
    match cli.command {
        Command::Serve { bind, db } => server::serve(bind, db).await?,
        Command::Mcp => mcp::run(client).await?,
        Command::Web {
            room,
            allow_approvals,
            bind,
            port,
            translator,
            language,
        } => {
            let mut settings = config.translation;
            if let Some(provider) = translator {
                settings.provider = provider;
            }
            if let Some(language) = language {
                settings.language = Some(language);
            }
            web::serve(client, room, bind, port, settings, allow_approvals).await?;
        }
        Command::Join { room, handle, id } => {
            let result = client
                .call(
                    "join",
                    json!({"room":room,"handle":handle,"id":id.unwrap_or_else(uuid::Uuid::new_v4)}),
                )
                .await?;
            println!("{}", serde_json::to_string_pretty(&result)?);
        }
        Command::Send {
            room,
            payload,
            codec,
            gloss,
            mention,
        } => {
            let payload: Value = serde_json::from_str(&payload)
                .context("payload must be JSON; wrap plain text in JSON quotes")?;
            println!("{}", client.call("send", json!({"room":room,"payload":payload,"codec":codec,"gloss":gloss,"mentions":mention})).await?);
        }
        Command::Codec {
            room,
            name,
            template,
        } => println!(
            "{}",
            client
                .call(
                    "codec",
                    json!({"room":room,"name":name,"template":template})
                )
                .await?
        ),
        Command::History {
            room,
            mut after,
            limit,
            mentions,
            raw,
            follow,
            translator,
            language,
            refresh_translations,
        } => {
            let mut settings = config.translation;
            if let Some(provider) = translator {
                settings.provider = provider;
            }
            if let Some(language) = language {
                settings.language = Some(language);
            }
            let mut settings = Some(settings);
            let mut engine = None;
            let mut stream: Option<Events> = None;
            let mut legacy = false;
            loop {
                let args = json!({"room":room,"after":after,"limit":limit,"include_human":!raw});
                let mut args = args;
                if let Some(id) = mentions {
                    args["mentions"] = json!(id);
                }
                let result = if follow && !legacy {
                    if stream.is_none() {
                        match client.events(&args).await {
                            Ok(Some(events)) => stream = Some(events),
                            Ok(None) => {
                                legacy = true;
                                eprintln!("旧サーバーのためポーリングに切り替えます");
                                continue;
                            }
                            Err(error) => {
                                if !transient(&error)
                                    && error.to_string() != "event connection timed out"
                                {
                                    return Err(error);
                                }
                                eprintln!("配信再接続待ち: {error}");
                                tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                                continue;
                            }
                        }
                    }
                    match stream.as_mut().unwrap().next().await {
                        Ok(Some(value)) => value,
                        result => {
                            if let Err(error) = result {
                                eprintln!("配信切断: {error}");
                            }
                            stream = None;
                            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                            continue;
                        }
                    }
                } else {
                    match client.call("history", json!({"room":room,"after":after,"limit":limit,"mentions":mentions,"include_human":!raw})).await {
                        Ok(value) => value,
                        Err(error) if follow && transient(&error) => {
                            eprintln!("履歴再接続待ち: {error}");
                            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                            continue;
                        }
                        Err(error) => return Err(error),
                    }
                };
                let messages = result["messages"].as_array().context("missing messages")?;
                let needs_translation = !raw
                    && messages
                        .iter()
                        .any(|m| m["human"].as_str().is_none_or(str::is_empty));
                if needs_translation && let Some(options) = settings.take() {
                    engine = translate::Translator::new(options, refresh_translations)?;
                }
                let mut failed = false;
                for message in messages {
                    after = message["id"].as_i64().context("missing message id")?;
                    if raw {
                        println!("{message}");
                        continue;
                    }
                    let human = if let Some(human) =
                        message["human"].as_str().filter(|s| !s.is_empty())
                    {
                        human.to_owned()
                    } else if let Some(engine) = &engine
                        && !failed
                    {
                        let previous = client.call("history", json!({"room":room,"before":message["id"],"limit":20,"include_human":true})).await?;
                        let context = previous["messages"].as_array().context("missing context")?;
                        let subsequent = client.call("history", json!({"room":room,"after":message["id"],"limit":20,"include_human":true})).await?;
                        let following = subsequent["messages"]
                            .as_array()
                            .context("missing following context")?;
                        match engine
                            .translate(
                                &client.base,
                                &room,
                                message,
                                context,
                                following,
                                &result["codecs"],
                            )
                            .await
                        {
                            Ok(translation) => {
                                format!("{}\n  原文: {}", translation.display(), message["payload"])
                            }
                            Err(error) => {
                                eprintln!("翻訳できませんでした: {error}");
                                failed = true;
                                format!("[未翻訳] {}", message["payload"])
                            }
                        }
                    } else {
                        format!("[未翻訳] {}", message["payload"])
                    };
                    println!(
                        "#{} {} {} ({}): {}",
                        message["id"],
                        message["created_at"].as_str().unwrap_or(""),
                        message["handle"].as_str().unwrap_or(""),
                        message["sender_ip"].as_str().unwrap_or(""),
                        human
                    );
                }
                if !follow {
                    break;
                }
                if legacy && messages.len() < limit as usize {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        }
    }
    Ok(())
}
