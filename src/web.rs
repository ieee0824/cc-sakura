use crate::{Client, translate};
use anyhow::{Context, Result, bail};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderValue, StatusCode, header},
    middleware::{self, Next},
    response::{Html, IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

type ApiResult = std::result::Result<Json<Value>, (StatusCode, Json<Value>)>;
struct TranslationState {
    settings: Option<translate::Settings>,
    engine: Option<translate::Translator>,
}
struct Viewer {
    client: Client,
    room: String,
    origin: String,
    translation: Mutex<TranslationState>,
    translating: Arc<tokio::sync::Semaphore>,
}

pub async fn serve(
    client: Client,
    room: String,
    port: u16,
    settings: translate::Settings,
) -> Result<()> {
    if client.session.is_none() {
        bail!(
            "HTML viewer needs a session for this room; join first, then configure session or pass --session"
        );
    }
    // Fail early on a wrong session or unreachable server, before advertising a viewer URL.
    client
        .call("history", json!({"room":room,"limit":1}))
        .await?;
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await?;
    let origin = format!("http://{}", listener.local_addr()?);
    let capability = uuid::Uuid::new_v4();
    let prefix = format!("/{capability}");
    let state = Arc::new(Viewer {
        client,
        room,
        origin: origin.clone(),
        translation: Mutex::new(TranslationState {
            settings: Some(settings),
            engine: None,
        }),
        translating: Arc::new(tokio::sync::Semaphore::new(1)),
    });
    let routes = Router::new()
        .route(
            "/app.css",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                    include_str!("web/app.css"),
                )
            }),
        )
        .route(
            "/app.js",
            get(|| async {
                (
                    [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                    include_str!("web/app.js"),
                )
            }),
        )
        .route("/threads.js", get(|| async { ([(header::CONTENT_TYPE,"text/javascript; charset=utf-8")],include_str!("web/threads.js")) }))
        .route("/icon.svg", get(|| async { ([(header::CONTENT_TYPE,"image/svg+xml")], r##"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 32 32"><rect width="32" height="32" rx="8" fill="#fbfaf7"/><text x="16" y="25" text-anchor="middle" font-size="28" fill="#b8776b">✳</text></svg>"##) }))
        .route("/api/info", get(info))
        .route("/api/history", get(history))
        .route("/api/events", get(events))
        .route("/api/message/{id}", get(message))
        .route("/api/translate/{id}", post(interpret));
    let app = Router::new()
        .route(
            &format!("{prefix}/"),
            get(|| async { Html(include_str!("web/index.html")) }),
        )
        .nest(&prefix, routes)
        .layer(middleware::from_fn_with_state(state.clone(), guard))
        .with_state(state);
    // Trailing slash makes relative asset/API URLs stay inside the capability path.
    println!("HTML viewer: {origin}{prefix}/");
    eprintln!(
        "ブラウザで上のURLを開いてください。終了: Ctrl-C（URLを知るローカルの利用者は閲覧できます）"
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

async fn guard(
    State(state): State<Arc<Viewer>>,
    request: axum::extract::Request,
    next: Next,
) -> Response {
    let expected_host = state.origin.trim_start_matches("http://");
    let headers = request.headers();
    let wrong_host = headers.get(header::HOST).and_then(|h| h.to_str().ok()) != Some(expected_host);
    let wrong_origin = headers
        .get(header::ORIGIN)
        .is_some_and(|h| h.to_str().ok() != Some(state.origin.as_str()));
    // Opening the viewer from a link may be a cross-site top-level navigation.
    // Reading APIs, embeds and model actions must still originate in the viewer.
    let navigation = request.method() == axum::http::Method::GET
        && headers
            .get("sec-fetch-mode")
            .is_some_and(|h| h == "navigate")
        && headers
            .get("sec-fetch-dest")
            .is_some_and(|h| h == "document")
        && request.uri().path().ends_with('/');
    let cross_site = headers
        .get("sec-fetch-site")
        .is_some_and(|h| h == "cross-site")
        && !navigation;
    let missing_action_header = request.method() == axum::http::Method::POST
        && headers.get("x-sakura-viewer").and_then(|h| h.to_str().ok()) != Some("1");
    let mut response = if wrong_host || wrong_origin || cross_site || missing_action_header {
        (
            StatusCode::FORBIDDEN,
            "This viewer accepts same-origin localhost requests only",
        )
            .into_response()
    } else {
        next.run(request).await
    };
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(header::CONTENT_SECURITY_POLICY,HeaderValue::from_static("default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self'; base-uri 'none'; frame-ancestors 'none'; form-action 'none'"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    headers.insert(header::X_FRAME_OPTIONS, HeaderValue::from_static("DENY"));
    response
}
async fn info(State(state): State<Arc<Viewer>>) -> Json<Value> {
    Json(json!({"room":state.room,"updates":"sse","poll_seconds":5}))
}
#[derive(Deserialize)]
struct Cursor {
    after: Option<i64>,
    before: Option<i64>,
}
fn failure(error: anyhow::Error) -> (StatusCode, Json<Value>) {
    eprintln!("HTML viewer: {error:#}");
    (
        StatusCode::BAD_GATEWAY,
        Json(
            json!({"error":"取得または翻訳に失敗しました。クライアントのログと接続設定を確認してください。"}),
        ),
    )
}
async fn events(
    State(state): State<Arc<Viewer>>,
    Query(cursor): Query<Cursor>,
    headers: axum::http::HeaderMap,
) -> Result<impl IntoResponse, (StatusCode, Json<Value>)> {
    let resumed = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0);
    let args = json!({"room":state.room,"after":cursor.after.unwrap_or(0).max(resumed),"include_human":true});
    let upstream = state
        .client
        .events(&args)
        .await
        .map_err(failure)?
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({"error":"legacy server","legacy":true})),
            )
        })?;
    let stream = futures_util::stream::unfold(upstream, |mut upstream| async move {
        match upstream.next().await {
            Ok(Some(value)) => {
                let id = value["messages"].as_array()?.last()?["id"].as_i64()?;
                let event = axum::response::sse::Event::default()
                    .event("messages")
                    .id(id.to_string())
                    .data(value.to_string());
                Some((Ok::<_, std::convert::Infallible>(event), upstream))
            }
            _ => None,
        }
    });
    Ok(axum::response::sse::Sse::new(stream).keep_alive(axum::response::sse::KeepAlive::default()))
}
async fn history(State(state): State<Arc<Viewer>>, Query(cursor): Query<Cursor>) -> ApiResult {
    if cursor.after.is_some() && cursor.before.is_some() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"afterとbeforeは同時に指定できません"})),
        ));
    }
    let mut args = json!({"room":state.room,"limit":100,"include_human":true});
    if let Some(after) = cursor.after {
        args["after"] = json!(after);
    } else {
        args["before"] = json!(cursor.before.unwrap_or(i64::MAX));
    }
    let result = state.client.call("history", args).await.map_err(failure)?;
    // Do not expose the central endpoint or session; only the room's display data.
    Ok(Json(result))
}
async fn message(State(state): State<Arc<Viewer>>, Path(id): Path<i64>) -> ApiResult {
    if id < 1 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"invalid message id"})),
        ));
    }
    let result = state
        .client
        .call(
            "history",
            json!({"room":state.room,"after":id-1,"limit":1,"include_human":true}),
        )
        .await
        .map_err(failure)?;
    let message = result["messages"]
        .as_array()
        .and_then(|ms| ms.first())
        .filter(|m| m["id"].as_i64() == Some(id));
    match message {
        Some(message) => Ok(Json(json!({"message":message}))),
        None => Err((
            StatusCode::NOT_FOUND,
            Json(json!({"error":"このルームに返信先の投稿が見つかりません"})),
        )),
    }
}
async fn interpret(State(state): State<Arc<Viewer>>, Path(id): Path<i64>) -> ApiResult {
    if id < 1 {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"invalid message id"})),
        ));
    }
    let permit = state.translating.clone().try_acquire_owned().map_err(|_| {
        (
            StatusCode::TOO_MANY_REQUESTS,
            Json(json!({"error":"ほかの翻訳が実行中です。完了後に試してください。"})),
        )
    })?;
    // rusqlite's local cache stays on a blocking thread. Model execution never blocks
    // history polling, and only one explicit translation can run at a time.
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let runtime = tokio::runtime::Handle::current();
        let result = runtime.block_on(state.client.call(
            "history",
            json!({"room":state.room,"after":id-1,"limit":1,"include_human":true}),
        ))?;
        let message = result["messages"]
            .as_array()
            .and_then(|m| m.first())
            .filter(|m| m["id"].as_i64() == Some(id))
            .context("message not found in this room")?;
        if let Some(human) = message["human"].as_str().filter(|s| !s.is_empty()) {
            return Ok(Json(json!({"status":"provided","text":human})));
        }
        let mut translation = state
            .translation
            .lock()
            .map_err(|_| anyhow::anyhow!("translator unavailable"))?;
        if let Some(settings) = translation.settings.take() {
            translation.engine = translate::Translator::new(settings, false)?;
        }
        let engine = translation
            .engine
            .as_ref()
            .context("translation disabled or CLI not installed")?;
        let previous = runtime.block_on(state.client.call(
            "history",
            json!({"room":state.room,"before":id,"limit":20,"include_human":true}),
        ))?;
        let following = runtime.block_on(state.client.call(
            "history",
            json!({"room":state.room,"after":id,"limit":20,"include_human":true}),
        ))?;
        let answer = runtime.block_on(
            engine.translate(
                &state.client.base,
                &state.room,
                message,
                previous["messages"].as_array().context("missing context")?,
                following["messages"]
                    .as_array()
                    .context("missing context")?,
                &result["codecs"],
            ),
        )?;
        Ok::<_, anyhow::Error>(Json(serde_json::to_value(answer)?))
    })
    .await
    .map_err(|e| failure(e.into()))?
    .map_err(failure)
}
