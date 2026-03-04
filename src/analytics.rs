use reqwest::Client;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::env;
use std::sync::LazyLock;
use std::time::Duration;

#[derive(Clone)]
pub struct Analytics {
	pub enabled: bool,
	pub host: String,
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
		let api_key = env::var("POSTHOG_API_KEY").unwrap_or_default();
		let client = Client::builder()
			.timeout(Duration::from_millis(1500))
			.build()
			.expect("analytics client");

		Self {
			enabled,
			host,
			api_key,
			client,
		}
	}

	pub async fn capture_pageview(&self, path: &str, user_agent: &str, ip: &str, host: &str) {
		if !self.enabled || self.api_key.is_empty() || self.host.is_empty() {
			return;
		}

		let mut hasher = Sha256::new();
		hasher.update(ip.as_bytes());
		hasher.update(user_agent.as_bytes());
		let distinct_id = format!("{:x}", hasher.finalize());
		let site_host = if host.is_empty() { "localhost" } else { host };
		let payload = json!({
			"api_key": self.api_key,
			"event": "$pageview",
			"distinct_id": distinct_id,
			"properties": {
				"$pathname": path,
				"$current_url": format!("https://{}{}", site_host, path),
				"$host": site_host,
				"$user_agent": user_agent
			}
		});

		let url = format!("{}/i/v0/e/", self.host.trim_end_matches('/'));
		let _ = self
			.client
			.post(url)
			.header("Content-Type", "application/json")
			.json(&payload)
			.send()
			.await;
	}
}

pub static ANALYTICS: LazyLock<Analytics> = LazyLock::new(Analytics::from_env);
