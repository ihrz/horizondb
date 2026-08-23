use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::{
    error::AppError,
    models::{PacketMessage, ResponseMessage},
    state::AppState,
    values,
};

const DEFAULT_TTL_MS: u64 = 60_000;
const MAX_TTL_MS: u64 = 30 * 24 * 60 * 60 * 1000;

pub async fn dispatch(state: &AppState, packet: PacketMessage) -> ResponseMessage {
    state.metrics.request_started();
    let id = packet.id.clone();
    match route(state, packet).await {
        Ok(data) => ResponseMessage::success(id, data),
        Err(error) => {
            state.metrics.request_failed();
            if error.is_internal() {
                tracing::error!(%error, "internal request failure");
            } else {
                tracing::debug!(%error, "request rejected");
            }
            ResponseMessage::failure(id, error.client_message())
        }
    }
}

async fn route(state: &AppState, packet: PacketMessage) -> Result<Value, AppError> {
    match packet.operation.as_str() {
        "login" => login(state, &packet).await,
        "logout" => logout(state, &packet).await,
        operation => {
            if !state.sessions.verify(packet.session_id.as_deref()).await {
                return Err(AppError::SessionExpired);
            }
            match operation {
                "get" => get_value(state, &packet).await,
                "set" => set_value(state, &packet).await,
                "delete" => delete_key(state, &packet).await,
                "add" => adjust_amount(state, &packet, 1.0).await,
                "sub" => adjust_amount(state, &packet, -1.0).await,
                "push" => push_element(state, &packet).await,
                "pull" => pull_element(state, &packet).await,
                "has" => has_key(state, &packet).await,
                "all" => list_all(state, &packet).await,
                "deleteAll" => delete_all(state, &packet).await,
                "cache" => cache_with_ttl(state, &packet).await,
                unknown => Err(AppError::validation(format!(
                    "Unknown operation: {unknown}"
                ))),
            }
        }
    }
}

fn table_of(packet: &PacketMessage) -> &str {
    packet.table.as_deref().unwrap_or(values::DEFAULT_TABLE)
}

fn scope(packet: &PacketMessage) -> Result<(&str, String, Vec<String>), AppError> {
    let raw_key = packet
        .key
        .as_deref()
        .filter(|key| !key.is_empty())
        .ok_or_else(|| AppError::validation("key is required"))?;
    let (base, path) = values::parse_key(raw_key)?;
    Ok((table_of(packet), base, path))
}

async fn login(state: &AppState, packet: &PacketMessage) -> Result<Value, AppError> {
    if state
        .authenticator
        .verify(packet.login.as_deref(), packet.password.as_deref())
    {
        let session_id = state.sessions.create().await;
        Ok(json!({ "success": true, "sessionId": session_id }))
    } else {
        Err(AppError::Auth)
    }
}

async fn logout(state: &AppState, packet: &PacketMessage) -> Result<Value, AppError> {
    state.sessions.revoke(packet.session_id.as_deref()).await;
    Ok(json!({ "success": true }))
}

async fn get_value(state: &AppState, packet: &PacketMessage) -> Result<Value, AppError> {
    let (table, base, path) = scope(packet)?;
    let default_value = packet.default_value.clone();

    if let Some(cached) = state.cache.get(table, &base).await {
        state.metrics.cache_hit();
        return Ok(project(&cached, &path, default_value));
    }
    state.metrics.cache_miss();

    match state.repository.get(table, &base).await? {
        Some(value) => {
            state.cache.insert(table, &base, value.clone(), None).await;
            Ok(project(&value, &path, default_value))
        }
        None => Ok(default_value.unwrap_or(Value::Null)),
    }
}

fn project(value: &Value, path: &[String], default: Option<Value>) -> Value {
    if path.is_empty() {
        return value.clone();
    }
    match values::get_nested(value, path) {
        Some(found) => found.clone(),
        None => default.unwrap_or(Value::Null),
    }
}

async fn set_value(state: &AppState, packet: &PacketMessage) -> Result<Value, AppError> {
    let (table, base, path) = scope(packet)?;
    let value = packet.value.clone().unwrap_or(Value::Null);

    if path.is_empty() {
        state.repository.set(table, &base, &value).await?;
        state.cache.insert(table, &base, value, None).await;
    } else {
        let root = state
            .repository
            .mutate(table, &base, json!({}), |mut doc| {
                values::set_nested(&mut doc, &path, value.clone())?;
                Ok(Some(doc))
            })
            .await?;
        state.cache.insert(table, &base, root, None).await;
    }
    Ok(json!({ "success": true }))
}

async fn adjust_amount(
    state: &AppState,
    packet: &PacketMessage,
    sign: f64,
) -> Result<Value, AppError> {
    let (table, base, path) = scope(packet)?;
    let requested = packet.amount.unwrap_or(1.0);
    if !requested.is_finite() {
        return Err(AppError::validation("amount must be finite"));
    }
    let delta = sign * requested;
    let absent = if path.is_empty() {
        Value::from(0.0_f64)
    } else {
        json!({})
    };

    let root = state
        .repository
        .mutate(table, &base, absent, |mut doc| {
            values::add_amount(&mut doc, &path, delta)?;
            Ok(Some(doc))
        })
        .await?;
    state.cache.insert(table, &base, root, None).await;
    Ok(json!({ "success": true }))
}

async fn delete_key(state: &AppState, packet: &PacketMessage) -> Result<Value, AppError> {
    let (table, base, path) = scope(packet)?;

    if path.is_empty() {
        state.repository.delete(table, &base).await?;
        state.cache.invalidate(table, &base).await;
    } else {
        let root = state
            .repository
            .mutate(table, &base, json!({}), |mut doc| {
                values::delete_nested(&mut doc, &path);
                if doc.as_object().is_some_and(serde_json::Map::is_empty) {
                    Ok(None)
                } else {
                    Ok(Some(doc))
                }
            })
            .await?;
        if root.is_null() {
            state.cache.invalidate(table, &base).await;
        } else {
            state.cache.insert(table, &base, root, None).await;
        }
    }
    Ok(json!({ "success": true }))
}

async fn push_element(state: &AppState, packet: &PacketMessage) -> Result<Value, AppError> {
    let (table, base, path) = scope(packet)?;
    let element = packet.element.clone().unwrap_or(Value::Null);

    let root = state
        .repository
        .mutate(table, &base, json!([]), |mut doc| {
            values::push_element(&mut doc, &path, element.clone())?;
            Ok(Some(doc))
        })
        .await?;
    state.cache.insert(table, &base, root, None).await;
    Ok(json!({ "success": true }))
}

async fn pull_element(state: &AppState, packet: &PacketMessage) -> Result<Value, AppError> {
    let (table, base, path) = scope(packet)?;
    let element = packet.element.clone().unwrap_or(Value::Null);

    let root = state
        .repository
        .mutate(table, &base, json!([]), |mut doc| {
            values::pull_element(&mut doc, &path, &element)?;
            Ok(Some(doc))
        })
        .await?;
    state.cache.insert(table, &base, root, None).await;
    Ok(json!({ "success": true }))
}

async fn has_key(state: &AppState, packet: &PacketMessage) -> Result<Value, AppError> {
    let (table, base, path) = scope(packet)?;

    if let Some(cached) = state.cache.get(table, &base).await {
        state.metrics.cache_hit();
        return Ok(json!(path_exists(&cached, &path)));
    }
    state.metrics.cache_miss();

    let exists = match state.repository.get(table, &base).await? {
        Some(value) => path_exists(&value, &path),
        None => false,
    };
    Ok(json!(exists))
}

fn path_exists(value: &Value, path: &[String]) -> bool {
    if path.is_empty() {
        return !value.is_null();
    }
    values::get_nested(value, path).is_some_and(|found| !found.is_null())
}

async fn list_all(state: &AppState, packet: &PacketMessage) -> Result<Value, AppError> {
    let entries = state.repository.list_all(table_of(packet)).await?;
    Ok(entries
        .into_iter()
        .map(|(id, value)| json!({ "id": id, "value": value }))
        .collect())
}

async fn delete_all(state: &AppState, packet: &PacketMessage) -> Result<Value, AppError> {
    let table = table_of(packet);
    state.repository.clear_table(table).await?;
    state.cache.invalidate_table(table).await;
    Ok(json!({ "success": true }))
}

async fn cache_with_ttl(state: &AppState, packet: &PacketMessage) -> Result<Value, AppError> {
    let (table, base, path) = scope(packet)?;
    let ttl_ms = packet.time.unwrap_or(DEFAULT_TTL_MS);
    if ttl_ms == 0 || ttl_ms > MAX_TTL_MS {
        return Err(AppError::validation("ttl must be between 1ms and 30 days"));
    }
    let value = packet.value.clone().unwrap_or(Value::Null);

    let root = if path.is_empty() {
        state.repository.set(table, &base, &value).await?;
        value
    } else {
        state
            .repository
            .mutate(table, &base, json!({}), |mut doc| {
                values::set_nested(&mut doc, &path, value.clone())?;
                Ok(Some(doc))
            })
            .await?
    };

    let expires_at = Instant::now() + Duration::from_millis(ttl_ms);
    state
        .cache
        .insert(table, &base, root, Some(expires_at))
        .await;

    let repository = state.repository.clone();
    let cleanup_table = table.to_owned();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(ttl_ms)).await;
        if let Err(error) = repository.delete(&cleanup_table, &base).await {
            tracing::debug!(%error, "ttl cleanup failed");
        }
    });
    Ok(json!({ "success": true }))
}
