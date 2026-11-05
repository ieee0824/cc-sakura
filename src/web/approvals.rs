use super::*;

// Requests and decisions live in room history so the queue survives viewer restarts.
fn options(message: &Value) -> Option<Vec<String>> {
    let p = &message["payload"];
    if p["type"] != "approval_request" || p["title"].as_str()?.trim().is_empty() {
        return None;
    }
    let values = p["options"].as_array()?;
    if values.is_empty() || values.len() > 8 {
        return None;
    }
    let mut result = Vec::new();
    for value in values {
        let value = value.as_str()?;
        if value.trim().is_empty() || value.len() > 2000 || result.iter().any(|v| v == value) {
            return None;
        }
        result.push(value.to_owned());
    }
    Some(result)
}
#[derive(Default)]
pub(super) struct Cache {
    after: i64,
    messages: Vec<Value>,
}
async fn history(state: &Viewer) -> anyhow::Result<Vec<Value>> {
    let mut cache = state.approvals.lock().await;
    loop {
        let page = state
            .client
            .call(
                "history",
                json!({"room":state.room,"after":cache.after,"limit":1000}),
            )
            .await?;
        let rows = page["messages"].as_array().context("missing messages")?;
        if rows.is_empty() {
            break;
        }
        let next = rows
            .last()
            .and_then(|m| m["id"].as_i64())
            .context("missing cursor")?;
        if next <= cache.after {
            bail!("non-advancing cursor");
        }
        cache.messages.extend(
            rows.iter()
                .filter(|m| options(m).is_some() || m["payload"]["type"] == "approval_decision")
                .cloned(),
        );
        cache.after = next;
        if rows.len() < 1000 {
            break;
        }
    }
    Ok(cache.messages.clone())
}

fn decision<'a>(messages: &'a [Value], request: &Value) -> Option<&'a Value> {
    let choices = options(request)?;
    messages.iter().find(|m| {
        m["id"].as_i64() > request["id"].as_i64()
            && m["payload"]["type"] == "approval_decision"
            && m["payload"]["reply_to"] == request["id"]
            && m["payload"]["choice"]
                .as_str()
                .is_some_and(|c| choices.iter().any(|s| s == c))
    })
}
pub(super) async fn list(State(state): State<Arc<Viewer>>) -> ApiResult {
    let messages = history(&state).await.map_err(failure)?;
    let requests: Vec<_> = messages
        .iter()
        .filter(|m| options(m).is_some())
        .map(|m| json!({"request":m,"decision":decision(&messages,m)}))
        .collect();
    Ok(Json(
        json!({"enabled":state.allow_approvals,"requests":requests}),
    ))
}
#[derive(Deserialize)]
pub(super) struct Choice {
    choice: String,
}
pub(super) async fn decide(
    State(state): State<Arc<Viewer>>,
    Path(id): Path<i64>,
    Json(input): Json<Choice>,
) -> ApiResult {
    if !state.allow_approvals {
        return Err((
            StatusCode::FORBIDDEN,
            Json(json!({"error":"承認投稿は無効です。--allow-approvalsが必要です。"})),
        ));
    }
    // Serialize buttons/tabs. On an ambiguous send error the next attempt reads history first.
    let _lock = state.decisions.lock().await;
    let messages = history(&state).await.map_err(failure)?;
    let request = messages
        .iter()
        .find(|m| m["id"].as_i64() == Some(id))
        .filter(|m| options(m).is_some())
        .ok_or_else(|| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({"error":"承認依頼がありません"})),
            )
        })?;
    if let Some(existing) = decision(&messages, request) {
        return Ok(Json(json!({"status":"already_decided","message":existing})));
    }
    if !options(request).unwrap().contains(&input.choice) {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error":"選択肢が一致しません"})),
        ));
    }
    let payload = json!({"type":"approval_decision","reply_to":id,"title":request["payload"]["title"],"choice":input.choice,"source":"html_ui"});
    let sent = state
        .client
        .call(
            "send",
            json!({"room":state.room,"payload":payload,"mentions":[request["sender"]]}),
        )
        .await
        .map_err(failure)?;
    Ok(Json(json!({"status":"sent","id":sent["id"]})))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_explicit_requests_and_matching_later_choices_resolve() {
        let r = json!({"id":10,"payload":{"type":"approval_request","title":"実行許可","options":["承認","保留"]}});
        assert!(options(&r).is_some());
        assert!(options(&json!({"payload":{"message":"承認してください"}})).is_none());
        let wrong = json!({"id":11,"payload":{"type":"approval_decision","reply_to":10,"choice":"全部許可"}});
        assert!(decision(&[wrong], &r).is_none());
        let correct =
            json!({"id":12,"payload":{"type":"approval_decision","reply_to":10,"choice":"承認"}});
        assert!(decision(&[correct], &r).is_some());
    }
}
