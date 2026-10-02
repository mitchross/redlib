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
	/// Share of visitors whose events are sent (0.0-1.0). Session replay is never sampled.
	pub sample_rate: f64,
	/// Share of visitors whose sessions are recorded (0.0-1.0), independent of `sample_rate`.
	pub replay_sample_rate: f64,
	/// Also send a server-side `$pageview` per request; duplicates the browser's own pageview.
	pub server_pageviews: bool,
	pub client: Client,
}

impl Analytics {
	pub fn from_env() -> Self {
		let enabled = parse_flag(env::var("POSTHOG_ENABLED").ok().as_deref(), false);
		let host = env::var("POSTHOG_HOST").unwrap_or_default();
		let client_host = env::var("POSTHOG_CLIENT_HOST").unwrap_or_default();
		let api_key = env::var("POSTHOG_API_KEY").unwrap_or_default();
		let sample_rate = parse_sample_rate(env::var("POSTHOG_SAMPLE_RATE").ok().as_deref());
		let replay_sample_rate = parse_sample_rate(env::var("POSTHOG_REPLAY_SAMPLE_RATE").ok().as_deref());
		let server_pageviews = parse_flag(env::var("POSTHOG_SERVER_PAGEVIEWS").ok().as_deref(), true);
		let client = Client::builder().timeout(Duration::from_millis(1500)).build().expect("analytics client");

		Self {
			enabled,
			host,
			client_host,
			api_key,
			sample_rate,
			replay_sample_rate,
			server_pageviews,
			client,
		}
	}

	/// Whether `capture_pageview` would send anything. Lets callers skip
	/// building and spawning the event when it would be a no-op.
	pub fn captures_pageviews(&self) -> bool {
		self.enabled && self.server_pageviews && !self.api_key.is_empty() && !self.host.is_empty()
	}

	pub async fn capture_pageview(&self, path: &str, user_agent: &str, ip: &str, host: &str, referrer: &str) {
		if !self.captures_pageviews() {
			return;
		}

		let mut hasher = Sha256::new();
		hasher.update(ip.as_bytes());
		hasher.update(user_agent.as_bytes());
		let distinct_id = format!("{:x}", hasher.finalize());

		if !in_sample(&distinct_id, self.sample_rate) {
			return;
		}

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
				"sample_rate": self.sample_rate,
				"$lib": "redlib-server",
				"$lib_version": env!("CARGO_PKG_VERSION")
			}
		});

		let url = format!("{}/i/v0/e/", self.host.trim_end_matches('/'));
		let _ = self
			.client
			.post(url)
			.header("Content-Type", "application/json")
			.header("X-Forwarded-For", ip)
			.json(&payload)
			.send()
			.await;
	}
}

/// Parse an on/off env var; unset or unrecognised keeps the default.
fn parse_flag(value: Option<&str>, default: bool) -> bool {
	match value.map(|v| v.trim().to_ascii_lowercase()).as_deref() {
		Some("1" | "true" | "yes" | "on") => true,
		Some("0" | "false" | "no" | "off") => false,
		_ => default,
	}
}

/// Parse a 0.0-1.0 sample rate; unset or invalid means send everything.
fn parse_sample_rate(value: Option<&str>) -> f64 {
	value
		.and_then(|v| v.trim().parse::<f64>().ok())
		.filter(|r| r.is_finite())
		.map_or(1.0, |r| r.clamp(0.0, 1.0))
}

/// Keep a visitor when the first 8 hex digits of their id fall under the rate,
/// so every event from one visitor is either all kept or all dropped.
fn in_sample(distinct_id: &str, rate: f64) -> bool {
	if rate >= 1.0 {
		return true;
	}
	let bucket = distinct_id.get(..8).and_then(|h| u32::from_str_radix(h, 16).ok()).unwrap_or(0);
	(f64::from(bucket) / f64::from(u32::MAX)) < rate
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

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn sample_rate_parsing() {
		assert_eq!(parse_sample_rate(None), 1.0);
		assert_eq!(parse_sample_rate(Some("0.1")), 0.1);
		assert_eq!(parse_sample_rate(Some(" 0.25 ")), 0.25);
		assert_eq!(parse_sample_rate(Some("5")), 1.0);
		assert_eq!(parse_sample_rate(Some("-1")), 0.0);
		assert_eq!(parse_sample_rate(Some("nan")), 1.0);
		assert_eq!(parse_sample_rate(Some("abc")), 1.0);
	}

	#[test]
	fn flag_parsing() {
		assert!(parse_flag(None, true));
		assert!(!parse_flag(None, false));
		assert!(!parse_flag(Some("off"), true));
		assert!(!parse_flag(Some(" FALSE "), true));
		assert!(parse_flag(Some("on"), false));
		assert!(parse_flag(Some("maybe"), true));
	}

	#[test]
	fn sampling_is_per_visitor() {
		assert!(in_sample("ffffffff", 1.0));
		assert!(!in_sample("00000000", 0.0));
		assert!(in_sample("00000000", 0.1));
		assert!(!in_sample("ffffffff", 0.1));
		assert_eq!(in_sample("19999999", 0.1), in_sample("19999999", 0.1));
	}

	#[test]
	fn sampling_keeps_about_the_rate() {
		let kept = (0..10_000u32).filter(|i| in_sample(&format!("{:x}", Sha256::digest(i.to_le_bytes())), 0.1)).count();
		assert!((800..1200).contains(&kept), "kept {kept}");
	}
}
