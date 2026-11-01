mod mcp;
mod server;
mod translate;

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
impl Client {
    async fn call(&self, operation: &str, args: Value) -> Result<Value> {
        let mut req = self
            .http
            .post(format!("{}/api/{operation}", self.base))
            .json(&args);
        if let Some(token) = &self.session {
            req = req.bearer_auth(token);
        }
        let response = req.send().await.context("connect to sakura server")?;
        let status = response.status();
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
            loop {
                let result = client.call("history", json!({"room":room,"after":after,"limit":limit,"mentions":mentions,"include_human":!raw})).await?;
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
                if messages.len() < limit as usize {
                    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                }
            }
        }
    }
    Ok(())
}
