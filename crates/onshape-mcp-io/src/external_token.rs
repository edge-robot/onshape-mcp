//! External-token mode: no OAuth exchange, persistence, watcher, or retry.
//! The initial contract is deliberately limited to exact-target GETs.
use std::{fs::OpenOptions, io::Read, time::Duration};
use chrono::{DateTime, Utc};
use onshape_mcp_core::config::{AppConfig, ExternalTokenConfig, ResolvedAuth};
use onshape_client_core::request::ApiRequest;
use onshape_client_io::{ClientAuthConfig, ClientConfig, OnshapeClient};
use oauth2::AccessToken;
use serde::Deserialize;
use secrecy::{SecretString, ExposeSecret};
use super::{McpError, RawResponse, OnshapeMcpServer};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Credential {
    access_token: SecretString,
    expires_at: i64,
    scopes: Vec<String>,
}

fn id(value: &str) -> bool {
    value.len() == 24 && value.bytes().all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
pub fn validate_config(config: &ExternalTokenConfig) -> Result<(), &'static str> {
    if !config.file.is_absolute() || !id(&config.document_id) || !id(&config.workspace_id) || !id(&config.element_id) {
        return Err("external_token_configuration_invalid");
    }
    Ok(())
}

fn load(config: &ExternalTokenConfig) -> Result<Credential, &'static str> {
    use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
    validate_config(config)?;
    let fail = |_| "external_token_unavailable";
    if std::fs::canonicalize(&config.file).map_err(fail)? != config.file { return Err("external_token_unavailable"); }
    let parent = config.file.parent().ok_or("external_token_unavailable")?;
    let dir = std::fs::symlink_metadata(parent).map_err(fail)?;
    let owner = std::fs::metadata("/proc/self").map_err(fail)?.uid();
    if !dir.is_dir() || dir.uid() != owner || dir.mode() & 0o077 != 0 { return Err("external_token_unavailable"); }
    // O_NOFOLLOW on Linux: validate the opened inode, then read that same inode.
    let mut file = OpenOptions::new().read(true).custom_flags(0x20000).open(&config.file).map_err(fail)?;
    let meta = file.metadata().map_err(fail)?;
    if !meta.is_file() || meta.nlink() != 1 || meta.uid() != owner || meta.mode() & 0o077 != 0 || meta.len() > 16384 {
        return Err("external_token_unavailable");
    }
    let mut bytes = Vec::new();
    file.by_ref().take(16385).read_to_end(&mut bytes).map_err(fail)?;
    if bytes.len() > 16384 { return Err("external_token_unavailable"); }
    let credential: Credential = serde_json::from_slice(&bytes).map_err(|_| "external_token_unavailable")?;
    let token = credential.access_token.expose_secret();
    if token.is_empty() || token.len() > 8192 || token.chars().any(char::is_whitespace) ||
        !credential.scopes.iter().any(|s| s == "OAuth2Read") || credential.scopes.len() > 32 {
        return Err("external_token_unavailable");
    }
    if credential.expires_at <= Utc::now().timestamp_millis() + 30000 { return Err("external_token_expired"); }
    Ok(credential)
}

pub fn resolved_auth(config: &ExternalTokenConfig) -> ResolvedAuth {
    match load(config) {
        Ok(c) => ResolvedAuth::OAuthReady { expires_at: DateTime::from_timestamp_millis(c.expires_at) },
        Err(code) => ResolvedAuth::NotConfigured { configured_method: onshape_client_core::auth::AuthMethod::OAuth, detail: code.into() },
    }
}

fn allowed(config: &ExternalTokenConfig, request: &ApiRequest) -> bool {
    if request.method != http::Method::GET || request.body.is_some() || !request.headers.is_empty() || !request.query_params.is_empty() { return false; }
    let d = &config.document_id;
    let w = &config.workspace_id;
    let e = &config.element_id;
    let fixed = [format!("/documents/{d}"), format!("/documents/d/{d}/workspaces"),
        format!("/documents/d/{d}/w/{w}/currentmicroversion"), format!("/documents/d/{d}/w/{w}/elements"),
        format!("/partstudios/d/{d}/w/{w}/e/{e}/features")];
    if fixed.contains(&request.path) { return true; }
    let parts: Vec<_> = request.path.split('/').collect();
        (parts.len() == 7 && parts[1..4] == ["documents", "d", d] && parts[4] == "m" && id(parts[5]) && parts[6] == "elements") ||
    (parts.len() == 9 && parts[1..4] == ["partstudios", "d", d] && parts[4] == "m" && id(parts[5]) && parts[6] == "e" && parts[7] == e && parts[8] == "features")
}

pub async fn execute(config: &ExternalTokenConfig, timeout: Duration, request: &ApiRequest) -> Result<RawResponse, McpError> {
    if !allowed(config, request) { return Err(McpError::invalid_request("external_token_read_policy_denied", None)); }
    let c = load(config).map_err(|code| McpError::invalid_request(code, None))?;
    // Fixed provider origin. Each dispatch reloads the atomically rotated file.
    let client = OnshapeClient::new(ClientConfig { base_url: "https://cad.onshape.com/api/v10".into(),
        auth: ClientAuthConfig::Bearer { access_token: AccessToken::new(c.access_token.expose_secret().into()) }, timeout: Some(timeout) })
        .map_err(|_| McpError::internal_error("external_token_client_unavailable", None))?;
    let response = client.execute(request).await.map_err(|_| McpError::internal_error("external_token_request_failed", None))?;
    Ok(super::raw_response_from_api_response(response))
}

pub async fn run_http(name: &str, version: &str, config: AppConfig) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use rmcp::transport::streamable_http_server::{StreamableHttpService, session::local::LocalSessionManager};
    use axum::{Router, routing::get, http::StatusCode};
    use std::sync::Arc;
    let host = config.http.host.clone();
    let port = config.http.port;
    let public_url: url::Url = config.http.public_url.as_deref().ok_or("external_token_local_url_required")?.parse()?;
    if public_url.scheme() != "http" || public_url.path() != "/" || public_url.query().is_some() || !public_url.username().is_empty() || public_url.password().is_some() {
        return Err("external_token_local_url_invalid".into());
    }
    let cancel = tokio_util::sync::CancellationToken::new();
    let transport_config = super::streamable_http_server_config(&public_url, cancel.clone())?;
    let server = OnshapeMcpServer::new(name, version, config)?;
    let credential = server.config.external_token.clone().ok_or("external_token_configuration_invalid")?;
    // Clone the existing shared handler: no additional OAuth issuer/proxy/client.
    let validation = server.validation.shared().ok_or("external_token_validation_unavailable")?;
    let service = StreamableHttpService::new(move || Ok(OnshapeMcpServer {
        info: server.info.clone(), config: Arc::clone(&server.config), spec: Arc::clone(&server.spec),
        api_state: Arc::clone(&server.api_state), validation: super::ValidationStore::Shared(Arc::clone(&validation)),
        login_state: Arc::clone(&server.login_state), oauth_state: None,
    }), Arc::new(LocalSessionManager::default()), transport_config);
    let app = Router::new().nest_service("/mcp", service)
        .route("/health", get(|| async { StatusCode::OK }))
        .route("/health/ready", get(move || { let c = credential.clone(); async move {
            if load(&c).is_ok() { StatusCode::OK } else { StatusCode::SERVICE_UNAVAILABLE }
        }}));
    let listener = tokio::net::TcpListener::bind((host.as_str(), port)).await?;
    axum::serve(listener, app).with_graceful_shutdown(async move { let _ = tokio::signal::ctrl_c().await; cancel.cancel(); }).await?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    fn config(file: std::path::PathBuf) -> ExternalTokenConfig { ExternalTokenConfig {
        file, document_id: "a".repeat(24), workspace_id: "b".repeat(24), element_id: "c".repeat(24) } }
    fn publish(file: &std::path::Path, value: serde_json::Value) {
        std::fs::write(file, value.to_string()).unwrap();
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    #[test]
    fn credentials_missing_expired_rotated_and_refresh_isolated() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let c = config(dir.path().join("token.json"));
        assert!(load(&c).is_err());
        let mut value = serde_json::json!({"accessToken":"secret-a", "expiresAt":Utc::now().timestamp_millis()+120000, "scopes":["OAuth2Read"]});
        publish(&c.file, value.clone()); assert_eq!(load(&c).unwrap().access_token.expose_secret(), "secret-a");
        value["accessToken"] = "secret-b".into();
        let rotated = dir.path().join("rotated"); publish(&rotated, value.clone());
        std::fs::rename(rotated, &c.file).unwrap(); assert_eq!(load(&c).unwrap().access_token.expose_secret(), "secret-b");
        value["refreshToken"] = "never-accepted".into(); publish(&c.file, value.clone()); assert!(load(&c).is_err());
        value.as_object_mut().unwrap().remove("refreshToken"); value["expiresAt"] = 1.into(); publish(&c.file, value); assert_eq!(load(&c).err(), Some("external_token_expired"));
    }
    #[test]
    fn wrong_permissions_symlink_and_secret_errors() {
        let dir = tempfile::tempdir().unwrap(); std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let c = config(dir.path().join("token.json")); publish(&c.file, serde_json::json!({"accessToken":"secret-a","expiresAt":Utc::now().timestamp_millis()+120000,"scopes":["OAuth2Read"]}));
        std::fs::set_permissions(&c.file, std::fs::Permissions::from_mode(0o644)).unwrap(); assert_eq!(load(&c).err(),Some("external_token_unavailable"));
        let link=dir.path().join("link"); std::os::unix::fs::symlink(&c.file, &link).unwrap(); assert!(load(&config(link)).is_err());
    }
    #[test]
    fn requests_cannot_escape_target_or_write() {
        let c=config("/run/onshape/token.json".into());
        let mut r=ApiRequest {method:http::Method::GET, path:format!("/partstudios/d/{}/w/{}/e/{}/features",c.document_id,c.workspace_id,c.element_id), query_params:vec![], headers:http::HeaderMap::new(), body:None, content_type:None};
        assert!(allowed(&c,&r)); r.method=http::Method::POST; assert!(!allowed(&c,&r));
        r.method=http::Method::GET; r.path=r.path.replace(&c.workspace_id,&"d".repeat(24)); assert!(!allowed(&c,&r));
        r.path=format!("/documents/d/{}/m/{}/elements",c.document_id,"e".repeat(24)); assert!(allowed(&c,&r));
        r.path.push_str("/../workspaces"); assert!(!allowed(&c,&r));
    }
}
