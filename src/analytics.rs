use reqwest::Client;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::env;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};
use uuid::Uuid;

/// Session timeout: 30 minutes of inactivity starts a new session.
const SESSION_TIMEOUT: Duration = Duration::from_secs(30 * 60);

struct SessionEntry {
	session_id: String,
	last_seen: Instant,
}

/// Tracks active sessions by distinct_id. Entries expire after SESSION_TIMEOUT.
static SESSIONS: LazyLock<Mutex<HashMap<String, SessionEntry>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

/// Get or create a session ID for a given distinct_id.
/// Returns a new UUID if no session exists or the previous one expired.
fn get_session_id(distinct_id: &str) -> String {
	let mut sessions = SESSIONS.lock().unwrap_or_else(|e| e.into_inner());
	let now = Instant::now();

	// Prune expired sessions periodically (every call is cheap enough for low-mid traffic)
	if sessions.len() > 1000 {
		sessions.retain(|_, entry| now.duration_since(entry.last_seen) < SESSION_TIMEOUT);
	}

	let entry = sessions.entry(distinct_id.to_owned()).or_insert_with(|| SessionEntry {
		session_id: Uuid::new_v4().to_string(),
		last_seen: now,
	});

	if now.duration_since(entry.last_seen) >= SESSION_TIMEOUT {
		entry.session_id = Uuid::new_v4().to_string();
	}
	entry.last_seen = now;

	entry.session_id.clone()
}

#[derive(Clone)]
pub struct Analytics {
	pub enabled: bool,
	pub host: String,
	pub client_host: String,
	pub api_key: String,
	pub client: Client,
}

impl Analytics {
	pub fn from_env() -> Self {
		let enabled = env::var("POSTHOG_ENABLED")
			.map(|v| {
				let val = v.to_ascii_lowercase();
				val == "1" || val == "true" || val == "yes" || val == "on"
			})
			.unwrap_or(false);
		let host = env::var("POSTHOG_HOST").unwrap_or_default();
		let client_host = env::var("POSTHOG_CLIENT_HOST").unwrap_or_default();
		let api_key = env::var("POSTHOG_API_KEY").unwrap_or_default();
		let client = Client::builder().timeout(Duration::from_millis(1500)).build().expect("analytics client");

		Self {
			enabled,
			host,
			client_host,
			api_key,
			client,
		}
	}

	pub async fn capture_pageview(&self, path: &str, user_agent: &str, ip: &str, host: &str, referrer: &str) {
		if !self.enabled || self.api_key.is_empty() || self.host.is_empty() {
			return;
		}

		let mut hasher = Sha256::new();
		hasher.update(ip.as_bytes());
		hasher.update(user_agent.as_bytes());
		let distinct_id = format!("{:x}", hasher.finalize());

		let session_id = get_session_id(&distinct_id);

		let site_host = if host.is_empty() { "localhost" } else { host };
		let current_url = format!("https://{}{}", site_host, path);

		let payload = json!({
			"api_key": self.api_key,
			"event": "$pageview",
			"distinct_id": distinct_id,
			"properties": {
				"$session_id": session_id,
				"$window_id": session_id,
				"$pathname": path,
				"$current_url": current_url,
				"$host": site_host,
				"$user_agent": user_agent,
				"$referrer": referrer,
				"$referring_domain": extract_domain(referrer),
				"$lib": "redlib-server",
				"$lib_version": env!("CARGO_PKG_VERSION")
			}
		});

		let url = format!("{}/i/v0/e/", self.host.trim_end_matches('/'));
		let _ = self.client.post(url).header("Content-Type", "application/json").header("X-Forwarded-For", ip).json(&payload).send().await;
	}
}

/// Extract domain from a referrer URL, or return empty string.
fn extract_domain(referrer: &str) -> &str {
	if referrer.is_empty() {
		return "";
	}
	// Skip past "https://" or "http://"
	let without_scheme = referrer.strip_prefix("https://").or_else(|| referrer.strip_prefix("http://")).unwrap_or(referrer);
	// Take everything before the first '/'
	without_scheme.split('/').next().unwrap_or("")
}

pub static ANALYTICS: LazyLock<Analytics> = LazyLock::new(Analytics::from_env);
