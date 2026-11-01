use anyhow::{Context, Result, bail};
use axum::{
    Json, Router,
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode},
    routing::post,
};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use std::{
    net::SocketAddr,
    path::PathBuf,
    sync::{Arc, Mutex},
};
use uuid::Uuid;

type Db = Arc<Mutex<Connection>>;
fn database(path: PathBuf) -> Result<Connection> {
    let db = Connection::open(path)?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    db.execute_batch("PRAGMA journal_mode=WAL; PRAGMA foreign_keys=ON;
        CREATE TABLE IF NOT EXISTS members(room TEXT NOT NULL, id TEXT NOT NULL, handle TEXT NOT NULL, token TEXT UNIQUE NOT NULL, PRIMARY KEY(room,id));
        CREATE TABLE IF NOT EXISTS codecs(room TEXT NOT NULL, name TEXT NOT NULL, template TEXT NOT NULL, PRIMARY KEY(room,name));
        CREATE TABLE IF NOT EXISTS messages(id INTEGER PRIMARY KEY AUTOINCREMENT, room TEXT NOT NULL, sender TEXT NOT NULL, handle TEXT NOT NULL, sender_ip TEXT NOT NULL, payload TEXT NOT NULL, codec TEXT, human TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT (strftime('%Y-%m-%dT%H:%M:%fZ','now')));
        CREATE INDEX IF NOT EXISTS messages_room_id ON messages(room,id);
        CREATE TABLE IF NOT EXISTS mentions(message_id INTEGER NOT NULL REFERENCES messages(id), member TEXT NOT NULL, PRIMARY KEY(message_id,member));")?;
    Ok(db)
}
pub async fn serve(bind: SocketAddr, path: PathBuf) -> Result<()> {
    let db = Arc::new(Mutex::new(database(path)?));
    let app = Router::new()
        .route("/api/{operation}", post(api))
        .with_state(db);
    let listener = tokio::net::TcpListener::bind(bind).await?;
    eprintln!("sakura listening on {}", listener.local_addr()?);
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}
async fn api(
    State(db): State<Db>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Path(operation): Path<String>,
    headers: HeaderMap,
    Json(args): Json<Value>,
) -> (StatusCode, Json<Value>) {
    let token = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .map(str::to_owned);
    let result = tokio::task::spawn_blocking(move || {
        let mut db = db
            .lock()
            .map_err(|_| anyhow::anyhow!("database unavailable"))?;
        execute(
            &mut db,
            &operation,
            args,
            token.as_deref(),
            &peer.ip().to_string(),
        )
    })
    .await;
    match result {
        Ok(Ok(value)) => (StatusCode::OK, Json(value)),
        Ok(Err(e)) => (
            StatusCode::BAD_REQUEST,
            Json(json!({"error":e.to_string()})),
        ),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(json!({"error":"internal server error"})),
        ),
    }
}
fn text<'a>(args: &'a Value, key: &str) -> Result<&'a str> {
    let s = args[key]
        .as_str()
        .with_context(|| format!("{key} must be a string"))?;
    if s.trim().is_empty() || s.len() > 16384 {
        bail!("{key} must contain 1..16384 bytes");
    }
    Ok(s)
}
/// A small deterministic language: templates interpolate named JSON fields once.
/// No recursive substitution, execution, or dependence on changing dictionaries.
fn render(template: &str, payload: &Value) -> Result<String> {
    let mut result = String::new();
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        result.push_str(&rest[..start]);
        let end = rest[start + 1..]
            .find('}')
            .context("unclosed template placeholder")?
            + start
            + 1;
        let key = &rest[start + 1..end];
        let value = payload
            .get(key)
            .with_context(|| format!("missing payload field: {key}"))?;
        result.push_str(&match value {
            Value::String(s) => s.clone(),
            v => v.to_string(),
        });
        rest = &rest[end + 1..];
    }
    result.push_str(rest);
    Ok(result)
}
fn execute(
    db: &mut Connection,
    operation: &str,
    args: Value,
    token: Option<&str>,
    ip: &str,
) -> Result<Value> {
    let room = text(&args, "room")?;
    if operation == "join" {
        let id = Uuid::parse_str(text(&args, "id")?)?.to_string();
        let handle = text(&args, "handle")?;
        let exists: bool = db.query_row(
            "SELECT EXISTS(SELECT 1 FROM members WHERE room=?1 AND id=?2)",
            params![room, id],
            |r| r.get(0),
        )?;
        if exists {
            bail!("identity already joined; use its existing session or a new UUID");
        }
        let token = Uuid::new_v4().to_string();
        db.execute(
            "INSERT INTO members VALUES(?1,?2,?3,?4)",
            params![room, id, handle, token],
        )?;
        return Ok(json!({"id":id,"room":room,"handle":handle,"session":token}));
    }
    let (sender, handle): (String, String) = db
        .query_row(
            "SELECT id,handle FROM members WHERE room=?1 AND token=?2",
            params![room, token.unwrap_or("")],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .context("join this room and provide its session token")?;
    match operation {
        "codec" => {
            let name = text(&args, "name")?;
            let template = text(&args, "template")?;
            // Validate syntax even before there is a real payload.
            let mut probe = serde_json::Map::new();
            let mut rest = template;
            while let Some(start) = rest.find('{') {
                let end = rest[start + 1..]
                    .find('}')
                    .context("unclosed template placeholder")?
                    + start
                    + 1;
                let key = &rest[start + 1..end];
                if key.is_empty() || key.contains('{') {
                    bail!("invalid placeholder");
                }
                probe.insert(key.into(), json!(""));
                rest = &rest[end + 1..];
            }
            render(template, &Value::Object(probe))?;
            db.execute(
                "INSERT INTO codecs VALUES(?1,?2,?3)",
                params![room, name, template],
            )
            .context("codec name already exists; use a new versioned name")?;
            Ok(json!({"name":name,"template":template}))
        }
        "send" => {
            let payload = args.get("payload").context("payload is required")?;
            let codec = args["codec"].as_str();
            let human = if let Some(name) = codec {
                let template: String = db
                    .query_row(
                        "SELECT template FROM codecs WHERE room=?1 AND name=?2",
                        params![room, name],
                        |r| r.get(0),
                    )
                    .optional()?
                    .context("unknown codec")?;
                render(&template, payload)?
            } else if let Some(gloss) = args["gloss"].as_str().filter(|s| !s.trim().is_empty()) {
                gloss.into()
            } else {
                bail!("provide a codec or a human-readable gloss, including for plain text");
            };
            let mentions: Vec<Uuid> =
                serde_json::from_value(args.get("mentions").cloned().unwrap_or(json!([])))?;
            let tx = db.transaction()?;
            for id in &mentions {
                let exists: bool = tx.query_row(
                    "SELECT EXISTS(SELECT 1 FROM members WHERE room=?1 AND id=?2)",
                    params![room, id.to_string()],
                    |r| r.get(0),
                )?;
                if !exists {
                    bail!("mentioned identity is not in this room: {id}");
                }
            }
            tx.execute("INSERT INTO messages(room,sender,handle,sender_ip,payload,codec,human) VALUES(?1,?2,?3,?4,?5,?6,?7)", params![room,sender,handle,ip,payload.to_string(),codec,human])?;
            let id = tx.last_insert_rowid();
            for member in mentions {
                tx.execute(
                    "INSERT OR IGNORE INTO mentions VALUES(?1,?2)",
                    params![id, member.to_string()],
                )?;
            }
            tx.commit()?;
            Ok(json!({"id":id}))
        }
        "history" => {
            let after = args["after"].as_i64().unwrap_or(0);
            let limit = args["limit"].as_u64().unwrap_or(100);
            if !(1..=1000).contains(&limit) {
                bail!("limit must be 1..1000");
            }
            let mentions = args["mentions"].as_str();
            if let Some(id) = mentions {
                Uuid::parse_str(id)?;
            }
            let mut stmt = db.prepare("SELECT id,sender,handle,sender_ip,payload,codec,human,created_at FROM messages WHERE room=?1 AND id>?2 AND (?3 IS NULL OR EXISTS(SELECT 1 FROM mentions WHERE message_id=messages.id AND member=?3)) ORDER BY id LIMIT ?4")?;
            let rows = stmt.query_map(params![room, after, mentions, limit], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, String>(4)?,
                    r.get::<_, Option<String>>(5)?,
                    r.get::<_, String>(6)?,
                    r.get::<_, String>(7)?,
                ))
            })?;
            let mut messages = Vec::new();
            for row in rows {
                let (id, sender, handle, ip, payload, codec, human, created_at) = row?;
                let mut ms =
                    db.prepare("SELECT member FROM mentions WHERE message_id=?1 ORDER BY member")?;
                let mentions = ms
                    .query_map([id], |r| r.get::<_, String>(0))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                let mut message = json!({"id":id,"sender":sender,"handle":handle,"sender_ip":ip,"payload":serde_json::from_str::<Value>(&payload)?,"codec":codec,"human":human,"created_at":created_at,"mentions":mentions});
                if !args["include_human"].as_bool().unwrap_or(false) {
                    message.as_object_mut().unwrap().remove("human");
                }
                messages.push(message);
            }
            let mut stmt =
                db.prepare("SELECT name,template FROM codecs WHERE room=?1 ORDER BY name")?;
            let codecs = stmt
                .query_map([room], |r| {
                    Ok(json!({"name":r.get::<_,String>(0)?,"template":r.get::<_,String>(1)?}))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(json!({"messages":messages,"codecs":codecs}))
        }
        _ => bail!("unknown operation"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn room_sessions_codec_history_and_mentions() -> Result<()> {
        let mut db = database(PathBuf::from(":memory:"))?;
        let id = Uuid::new_v4();
        let joined = execute(
            &mut db,
            "join",
            json!({"room":"r","id":id,"handle":"bot"}),
            None,
            "127.0.0.1",
        )?;
        let token = joined["session"].as_str();
        assert!(execute(&mut db, "history", json!({"room":"other"}), token, "ip").is_err());
        execute(
            &mut db,
            "codec",
            json!({"room":"r","name":"s1","template":"{t}: {s}"}),
            token,
            "ip",
        )?;
        assert!(
            execute(
                &mut db,
                "send",
                json!({"room":"r","payload":[1,2]}),
                token,
                "ip"
            )
            .is_err()
        );
        assert!(
            execute(
                &mut db,
                "send",
                json!({"room":"r","codec":"s1","payload":{"t":"build"}}),
                token,
                "ip"
            )
            .is_err()
        );
        execute(
            &mut db,
            "send",
            json!({"room":"r","codec":"s1","payload":{"t":"build","s":"ok"},"mentions":[id,id]}),
            token,
            "192.0.2.1",
        )?;
        assert!(
            execute(
                &mut db,
                "codec",
                json!({"room":"r","name":"s1","template":"changed"}),
                token,
                "ip"
            )
            .is_err()
        );
        let history = execute(
            &mut db,
            "history",
            json!({"room":"r","mentions":id,"include_human":true}),
            token,
            "ip",
        )?;
        assert_eq!(history["messages"][0]["human"], "build: ok");
        assert_eq!(history["messages"][0]["sender_ip"], "192.0.2.1");
        assert_eq!(
            history["messages"][0]["mentions"].as_array().unwrap().len(),
            1
        );
        assert!(
            execute(
                &mut db,
                "history",
                json!({"room":"r","after":1}),
                token,
                "ip"
            )?["messages"]
                .as_array()
                .unwrap()
                .is_empty()
        );
        Ok(())
    }
    #[test]
    fn substitutions_are_not_reinterpreted() {
        assert_eq!(
            render("{a}/{b}", &json!({"a":"{b}","b":42})).unwrap(),
            "{b}/42"
        );
    }
}
