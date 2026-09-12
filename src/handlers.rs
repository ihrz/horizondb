use std::time::{Duration, Instant};

use serde_json::{Value, json};

use crate::{
    db::Expiry,
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
    let started = Instant::now();
    tracing::debug!(
        operation = %packet.operation,
        table = %packet.table.as_deref().unwrap_or_default(),
        key = %packet.key.as_deref().unwrap_or_default(),
        %id,
        "request"
    );
    match route(state, packet).await {
        Ok(data) => {
            tracing::debug!(%id, elapsed = ?Instant::now().duration_since(started), "request ok");
            ResponseMessage::success(id, data)
        }
        Err(error) => {
            state.metrics.request_failed();
            if error.is_internal() {
                tracing::error!(
                    %id,
                    elapsed = ?Instant::now().duration_since(started),
                    %error,
                    "internal request failure"
                );
            } else {
                tracing::debug!(
                    %id,
                    elapsed = ?Instant::now().duration_since(started),
                    %error,
                    "request rejected"
                );
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
                "set" => set_value(state, packet).await,
                "delete" => delete_key(state, &packet).await,
                "add" => adjust_amount(state, &packet, 1.0).await,
                "sub" => adjust_amount(state, &packet, -1.0).await,
                "push" => push_element(state, packet).await,
                "pull" => pull_element(state, &packet).await,
                "has" => has_key(state, &packet).await,
                "all" => list_all(state, &packet).await,
                "deleteAll" => delete_all(state, &packet).await,
                "cache" => cache_with_ttl(state, packet).await,
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

fn wants_value(packet: &PacketMessage) -> bool {
    packet.return_value.unwrap_or(false)
}

async fn login(state: &AppState, packet: &PacketMessage) -> Result<Value, AppError> {
    // Password verification (argon2id in production) is CPU-bound: run it on
    // the blocking pool so a burst of logins after a reconnect cannot stall
    // the async workers that serve every other request.
    let authenticator = state.authenticator.clone();
    let username = packet.login.clone();
    let password = packet.password.clone();
    let verified = tokio::task::spawn_blocking(move || {
        authenticator.verify(username.as_deref(), password.as_deref())
    })
    .await
    .map_err(|error| AppError::internal(format!("login task failed: {error}")))?;

    if verified {
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
    let default_value = packet.default_value.as_ref();

    let found = state
        .repository
        .read(table, &base, |doc| project(doc, &path, default_value))
        .await?;
    Ok(found.unwrap_or_else(|| default_value.cloned().unwrap_or(Value::Null)))
}

/// Copies the part of `value` addressed by `path` (the whole value for an
/// empty path), falling back to `default` when the path does not resolve.
fn project(value: &Value, path: &[String], default: Option<&Value>) -> Value {
    if path.is_empty() {
        return value.clone();
    }
    match values::get_nested(value, path) {
        Some(found) => found.clone(),
        None => default.cloned().unwrap_or(Value::Null),
    }
}

async fn set_value(state: &AppState, packet: PacketMessage) -> Result<Value, AppError> {
    let (table, base, path) = scope(&packet)?;
    let value = packet.value.clone().unwrap_or(Value::Null);

    if path.is_empty() {
        state.repository.set(table, &base, value).await?;
    } else {
        state
            .repository
            .mutate(table, &base, json!({}), Expiry::Clear, |doc| {
                values::set_nested(doc, &path, value)?;
                Ok(Some(()))
            })
            .await?;
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

    let updated = state
        .repository
        .mutate(table, &base, absent, Expiry::Clear, |doc| {
            values::add_amount(doc, &path, delta)?;
            Ok(Some(project(doc, &path, None)))
        })
        .await?;
    Ok(json!({ "success": true, "value": updated.unwrap_or(Value::Null) }))
}

async fn delete_key(state: &AppState, packet: &PacketMessage) -> Result<Value, AppError> {
    let (table, base, path) = scope(packet)?;

    let existed = if path.is_empty() {
        state.repository.delete(table, &base).await?
    } else {
        let mut removed = false;
        state
            .repository
            .mutate(table, &base, json!({}), Expiry::Clear, |doc| {
                removed = values::delete_nested(doc, &path);
                if doc.as_object().is_some_and(serde_json::Map::is_empty) {
                    Ok(None)
                } else {
                    Ok(Some(()))
                }
            })
            .await?;
        removed
    };
    Ok(json!({ "success": true, "existed": existed }))
}

/// A missing document starts as an empty array for a top-level `push`/`pull`
/// and as an empty object when the array lives under a nested path (the
/// first `push` of `guild.STATS.USER.<id>.messages` must not fail because
/// the guild document does not exist yet).
fn array_absent(path: &[String]) -> Value {
    if path.is_empty() {
        json!([])
    } else {
        json!({})
    }
}

async fn push_element(state: &AppState, packet: PacketMessage) -> Result<Value, AppError> {
    let (table, base, path) = scope(&packet)?;
    let element = packet.element.clone().unwrap_or(Value::Null);
    let wants_value = wants_value(&packet);

    let updated = state
        .repository
        .mutate(table, &base, array_absent(&path), Expiry::Clear, |doc| {
            values::push_element(doc, &path, element)?;
            Ok(Some(wants_value.then(|| project(doc, &path, None))))
        })
        .await?;
    Ok(mutation_response(updated.flatten()))
}

async fn pull_element(state: &AppState, packet: &PacketMessage) -> Result<Value, AppError> {
    let (table, base, path) = scope(packet)?;
    let element = packet.element.clone().unwrap_or(Value::Null);
    let wants_value = wants_value(packet);

    let updated = state
        .repository
        .mutate(table, &base, array_absent(&path), Expiry::Clear, |doc| {
            values::pull_element(doc, &path, &element)?;
            Ok(Some(wants_value.then(|| project(doc, &path, None))))
        })
        .await?;
    Ok(mutation_response(updated.flatten()))
}

fn mutation_response(value: Option<Value>) -> Value {
    match value {
        Some(value) => json!({ "success": true, "value": value }),
        None => json!({ "success": true }),
    }
}

async fn has_key(state: &AppState, packet: &PacketMessage) -> Result<Value, AppError> {
    let (table, base, path) = scope(packet)?;

    let exists = state
        .repository
        .read(table, &base, |doc| path_exists(doc, &path))
        .await?
        .unwrap_or(false);
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
    Ok(json!({ "success": true }))
}

async fn cache_with_ttl(state: &AppState, packet: PacketMessage) -> Result<Value, AppError> {
    let (table, base, path) = scope(&packet)?;
    let ttl_ms = packet.time.unwrap_or(DEFAULT_TTL_MS);
    if ttl_ms == 0 || ttl_ms > MAX_TTL_MS {
        return Err(AppError::validation("ttl must be between 1ms and 30 days"));
    }
    let value = packet.value.clone().unwrap_or(Value::Null);
    let ttl = Duration::from_millis(ttl_ms);
    let expires_at = Instant::now() + ttl;

    if path.is_empty() {
        state
            .repository
            .set_with_ttl(table, &base, value, ttl)
            .await?;
    } else {
        state
            .repository
            .mutate(table, &base, json!({}), Expiry::Set(expires_at), |doc| {
                values::set_nested(doc, &path, value)?;
                Ok(Some(()))
            })
            .await?;
    }

    let repository = state.repository.clone();
    let cleanup_table = table.to_owned();
    tokio::spawn(async move {
        tokio::time::sleep(ttl).await;
        if let Err(error) = repository.delete_if_expired(&cleanup_table, &base).await {
            tracing::debug!(%error, "ttl cleanup failed");
        }
    });
    Ok(json!({ "success": true }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_copies_subtree_or_default() {
        let doc = json!({"a": {"b": [1, 2]}, "n": null});
        assert_eq!(project(&doc, &[], None), doc);
        assert_eq!(
            project(&doc, &["a".to_owned(), "b".to_owned()], None),
            json!([1, 2])
        );
        assert_eq!(project(&doc, &["zzz".to_owned()], None), Value::Null);
        assert_eq!(
            project(&doc, &["zzz".to_owned()], Some(&json!(7))),
            json!(7)
        );
    }

    #[test]
    fn path_exists_treats_null_as_absent() {
        let doc = json!({"a": {"b": 1}, "n": null});
        assert!(path_exists(&doc, &[]));
        assert!(path_exists(&doc, &["a".to_owned(), "b".to_owned()]));
        assert!(!path_exists(&doc, &["n".to_owned()]));
        assert!(!path_exists(&doc, &["missing".to_owned()]));
        assert!(!path_exists(&Value::Null, &[]));
    }

    #[test]
    fn array_absent_matches_path_shape() {
        assert_eq!(array_absent(&[]), json!([]));
        assert_eq!(array_absent(&["items".to_owned()]), json!({}));
    }

    #[test]
    fn mutation_response_only_carries_requested_values() {
        assert_eq!(mutation_response(None), json!({"success": true}));
        assert_eq!(
            mutation_response(Some(json!([1]))),
            json!({"success": true, "value": [1]})
        );
    }
}
