//! Network settings: the proxy and the user agent.
//!
//! * **Proxy**: the system's (default: environment variables, and the Internet Options on Windows), none, or
//!   one given by the user (`http`, `https`, `socks5`, `socks5h`) with an optional user name and password.
//!   The password is sealed on disk like cookies, never shown again, never logged.
//! * **User agent**: VeloxFetch's own, or one the user typed (a single printable line).
//!
//! A change builds a new HTTP client for the engine and the stream downloader. Transfers already running
//! finish with the client they started with; starts, resumes and retries use the new one.

use http_client::{BasicAuth, ClientConfig, HttpClient};

use super::*;

const MODE_KEY: &str = "network.proxy_mode";
const URL_KEY: &str = "network.proxy_url";
const USER_KEY: &str = "network.proxy_user";
const PASSWORD_KEY: &str = "network.proxy_password";
const UA_KEY: &str = "network.user_agent";
/// Binds the sealed password to its purpose: a sealed value copied into another setting does not open.
const PASSWORD_CONTEXT: &str = "network.proxy_password.v1";

#[derive(Clone, Default)]
pub(super) struct Network {
    pub mode: ProxyMode,
    pub url: Option<String>,
    pub user: Option<String>,
    pub password: Option<String>,
    pub user_agent: Option<String>,
}

impl Network {
    /// What was saved; anything unreadable (hand-edited, damaged, sealed with a lost key) is ignored.
    pub(super) fn load(store: &SqliteStore, sealer: Option<&secrets::Sealer>) -> Self {
        let get = |k: &str| store.get_setting(k).ok().flatten().filter(|v| !v.is_empty());
        let mode = match get(MODE_KEY).as_deref() {
            Some("none") => ProxyMode::None,
            Some("manual") => ProxyMode::Manual,
            _ => ProxyMode::System,
        };
        let url = get(URL_KEY).and_then(|u| validate::proxy_url(&u).ok());
        let user = get(USER_KEY).filter(|u| validate::credentials(u, "").is_ok());
        let password = get(PASSWORD_KEY)
            .and_then(|sealed| sealer?.open(&sealed, PASSWORD_CONTEXT))
            .and_then(|plain| String::from_utf8(plain.to_vec()).ok());
        let user_agent = get(UA_KEY).and_then(|u| validate::user_agent(&u).ok());
        // A manual proxy without an address cannot work: fall back to the system's.
        let mode = if mode == ProxyMode::Manual && url.is_none() { ProxyMode::System } else { mode };
        Network { mode, url, user, password, user_agent }
    }

    /// `base` with this proxy and user agent.
    pub(super) fn client_config(&self, base: &ClientConfig) -> ClientConfig {
        let mut c = base.clone();
        if let Some(ua) = &self.user_agent {
            c.user_agent = ua.clone();
        }
        match self.mode {
            ProxyMode::System => {}
            ProxyMode::None => {
                c.proxy = None;
                c.no_proxy = true;
            }
            ProxyMode::Manual => {
                c.proxy = self.url.clone();
                c.proxy_auth = self.user.clone().map(|username| BasicAuth { username, password: self.password.clone() });
            }
        }
        c
    }
}

impl DownloadManager {
    fn network(&self) -> std::sync::MutexGuard<'_, Network> {
        self.inner.network.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn get_network(&self) -> NetworkDto {
        let n = self.network();
        NetworkDto {
            proxy_mode: n.mode,
            proxy_url: n.url.clone(),
            proxy_username: n.user.clone(),
            proxy_has_password: n.password.is_some(),
            user_agent: n.user_agent.clone(),
            default_user_agent: self.inner.cfg.client.user_agent.clone(),
            secrets_protected: self.inner.sealer.is_some(),
        }
    }

    /// Change the proxy and the user agent. Everything is checked (and the HTTP client built) before
    /// anything is saved, so a bad value changes nothing.
    pub async fn set_network(&self, req: SetNetwork) -> Result<NetworkDto, ManagerError> {
        let text = |v: &Option<String>| v.as_deref().map(str::trim).filter(|v| !v.is_empty()).map(str::to_string);
        // The address matters only for a manual proxy; with another mode a half-typed one is simply not kept.
        let url = match req.proxy_mode {
            ProxyMode::Manual => text(&req.proxy_url).map(|u| validate::proxy_url(&u)).transpose()?,
            _ => text(&req.proxy_url).and_then(|u| validate::proxy_url(&u).ok()),
        };
        if req.proxy_mode == ProxyMode::Manual && url.is_none() {
            return Err(ManagerError::invalid("enter the proxy address, like http://proxy.example:3128"));
        }
        let user = text(&req.proxy_username);
        let password = match &req.proxy_password {
            None => self.network().password.clone(),
            Some(p) if p.is_empty() => None,
            Some(p) => Some(p.clone()),
        };
        // No user name: no password either.
        let password = if user.is_some() { password } else { None };
        if let Some(u) = &user {
            validate::credentials(u, password.as_deref().unwrap_or(""))?;
        }
        let user_agent = text(&req.user_agent).map(|u| validate::user_agent(&u)).transpose()?;
        let next = Network { mode: req.proxy_mode, url, user, password, user_agent };

        let client_cfg = next.client_config(&self.inner.cfg.client);
        let engine =
            HttpClient::new(&client_cfg).map_err(|e| ManagerError::invalid(format!("these network settings cannot be used: {e}")))?;
        let media =
            HttpClient::new(&client_cfg).map_err(|e| ManagerError::invalid(format!("these network settings cannot be used: {e}")))?;

        let mode = match next.mode {
            ProxyMode::System => "system",
            ProxyMode::None => "none",
            ProxyMode::Manual => "manual",
        };
        // Without a sealing key the password is kept in memory only (and asked again after a restart).
        let sealed = next.password.as_deref().and_then(|p| self.inner.sealer.as_ref().map(|s| s.seal(p.as_bytes(), PASSWORD_CONTEXT)));
        let values = [
            (MODE_KEY, mode.to_string()),
            (URL_KEY, next.url.clone().unwrap_or_default()),
            (USER_KEY, next.user.clone().unwrap_or_default()),
            (PASSWORD_KEY, sealed.unwrap_or_default()),
            (UA_KEY, next.user_agent.clone().unwrap_or_default()),
        ];
        let store = self.inner.store.clone();
        blocking(move || {
            for (k, v) in values {
                store.set_setting(k, &v)?;
            }
            Ok(())
        })
        .await?;

        self.inner.downloader.set_client(engine);
        *self.inner.media_http.write().unwrap_or_else(|p| p.into_inner()) = media;
        *self.network() = next;
        tracing::info!(mode, "network settings changed");
        Ok(self.get_network())
    }
}
