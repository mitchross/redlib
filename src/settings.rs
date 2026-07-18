#![allow(clippy::cmp_owned)]

use std::collections::HashMap;

// CRATES
use crate::server::ResponseExt;
use crate::subreddit::join_until_size_limit;
use crate::utils::{deflate_decompress, redirect, template, Preferences};
use askama::Template;
use cookie::Cookie;
use futures_lite::StreamExt;
use hyper::{Body, Request, Response};
use time::{Duration, OffsetDateTime};
use tokio::time::timeout;
use url::form_urlencoded;

// STRUCTS
#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsTemplate {
	prefs: Preferences,
	url: String,
}

// CONSTANTS

const PREFS: [&str; 21] = [
	"theme",
	"front_page",
	"layout",
	"wide",
	"comment_sort",
	"post_sort",
	"blur_spoiler",
	"show_nsfw",
	"blur_nsfw",
	"use_hls",
	"hide_hls_notification",
	"autoplay_videos",
	"hide_sidebar_and_summary",
	"fixed_navbar",
	"hide_awards",
	"hide_score",
	"disable_visit_reddit_confirmation",
	"video_quality",
	"remove_default_feeds",
	"post_count",
	"collapse_depth",
];

// FUNCTIONS

/// Retrieve cookies from request "Cookie" header
pub async fn get(req: Request<Body>) -> Result<Response<Body>, String> {
	let url = req.uri().to_string();
	Ok(template(&SettingsTemplate {
		prefs: Preferences::new(&req),
		url,
	}))
}

/// Set cookies using response "Set-Cookie" header
pub async fn set(req: Request<Body>) -> Result<Response<Body>, String> {
	// Split the body into parts
	let (parts, mut body) = req.into_parts();

	// Grab existing cookies
	let _cookies: Vec<Cookie<'_>> = parts
		.headers
		.get_all("Cookie")
		.iter()
		.filter_map(|header| Cookie::parse(header.to_str().unwrap_or_default()).ok())
		.collect();

	// Aggregate the body...
	// let whole_body = hyper::body::aggregate(req).await.map_err(|e| e.to_string())?;
	let body_bytes = body
		.try_fold(Vec::new(), |mut data, chunk| {
			data.extend_from_slice(&chunk);
			Ok(data)
		})
		.await
		.map_err(|e| e.to_string())?;

	let form = url::form_urlencoded::parse(&body_bytes).collect::<HashMap<_, _>>();

	let mut response = redirect("/settings");

	for &name in &PREFS {
		match form.get(name) {
			Some(value) => response.insert_cookie(
				Cookie::build((name.to_owned(), value.clone()))
					.path("/")
					.http_only(true)
					.expires(OffsetDateTime::now_utc() + Duration::weeks(52))
					.into(),
			),
			None => response.remove_cookie(name.to_string()),
		};
	}

	Ok(response)
}

fn set_cookies_method(req: Request<Body>, remove_cookies: bool) -> Response<Body> {
	// Split the body into parts
	let (parts, _) = req.into_parts();

	// Grab existing cookies
	let _cookies: Vec<Cookie<'_>> = parts
		.headers
		.get_all("Cookie")
		.iter()
		.filter_map(|header| Cookie::parse(header.to_str().unwrap_or_default()).ok())
		.collect();

	let query = parts.uri.query().unwrap_or_default().as_bytes();

	let form = url::form_urlencoded::parse(query).collect::<HashMap<_, _>>();

	let path = match form.get("redirect") {
		Some(value) => {
			let value = value.replace("%26", "&").replace("%23", "#");
			if value.starts_with('/') {
				value
			} else {
				format!("/{value}")
			}
		}
		None => "/".to_string(),
	};

	let mut response = redirect(&path);

	for name in PREFS {
		match form.get(name) {
			Some(value) => response.insert_cookie(
				Cookie::build((name.to_owned(), value.clone()))
					.path("/")
					.http_only(true)
					.expires(OffsetDateTime::now_utc() + Duration::weeks(52))
					.into(),
			),
			None => {
				if remove_cookies {
					response.remove_cookie(name.to_string());
				}
			}
		};
	}

	// Get subscriptions/filters to restore from query string
	let subscriptions = form.get("subscriptions");
	let filters = form.get("filters");

	// We can't search through the cookies directly like in subreddit.rs, so instead we have to make a string out of the request's headers to search through
	let cookies_string = parts
		.headers
		.get("cookie")
		.map(|hv| hv.to_str().unwrap_or("").to_string()) // Return String
		.unwrap_or_else(String::new); // Return an empty string if None

	// If there are subscriptions to restore set them and delete any old subscriptions cookies, otherwise delete them all
	if let Some(subscriptions) = subscriptions {
		let sub_list: Vec<String> = subscriptions.split('+').map(str::to_string).collect();

		// Start at 0 to keep track of what number we need to start deleting old subscription cookies from
		let mut subscriptions_number_to_delete_from = 0;

		// Starting at 0 so we handle the subscription cookie without a number first
		for (subscriptions_number, list) in join_until_size_limit(&sub_list).into_iter().enumerate() {
			let subscriptions_cookie = if subscriptions_number == 0 {
				"subscriptions".to_string()
			} else {
				format!("subscriptions{subscriptions_number}")
			};

			response.insert_cookie(
				Cookie::build((subscriptions_cookie, list))
					.path("/")
					.http_only(true)
					.expires(OffsetDateTime::now_utc() + Duration::weeks(52))
					.into(),
			);

			subscriptions_number_to_delete_from += 1;
		}

		// While subscriptionsNUMBER= is in the string of cookies add a response removing that cookie
		while cookies_string.contains(&format!("subscriptions{subscriptions_number_to_delete_from}=")) {
			// Remove that subscriptions cookie
			response.remove_cookie(format!("subscriptions{subscriptions_number_to_delete_from}"));

			// Increment subscriptions cookie number
			subscriptions_number_to_delete_from += 1;
		}
	} else {
		// Remove unnumbered subscriptions cookie
		response.remove_cookie("subscriptions".to_string());

		// Starts at one to deal with the first numbered subscription cookie and onwards
		let mut subscriptions_number_to_delete_from = 1;

		// While subscriptionsNUMBER= is in the string of cookies add a response removing that cookie
		while cookies_string.contains(&format!("subscriptions{subscriptions_number_to_delete_from}=")) {
			// Remove that subscriptions cookie
			response.remove_cookie(format!("subscriptions{subscriptions_number_to_delete_from}"));

			// Increment subscriptions cookie number
			subscriptions_number_to_delete_from += 1;
		}
	}

	// If there are filters to restore set them and delete any old filters cookies, otherwise delete them all
	if let Some(filters) = filters {
		let filters_list: Vec<String> = filters.split('+').map(str::to_string).collect();

		// Start at 0 to keep track of what number we need to start deleting old subscription cookies from
		let mut filters_number_to_delete_from = 0;

		// Starting at 0 so we handle the subscription cookie without a number first
		for (filters_number, list) in join_until_size_limit(&filters_list).into_iter().enumerate() {
			let filters_cookie = if filters_number == 0 {
				"filters".to_string()
			} else {
				format!("filters{filters_number}")
			};

			response.insert_cookie(
				Cookie::build((filters_cookie, list))
					.path("/")
					.http_only(true)
					.expires(OffsetDateTime::now_utc() + Duration::weeks(52))
					.into(),
			);

			filters_number_to_delete_from += 1;
		}

		// While filtersNUMBER= is in the string of cookies add a response removing that cookie
		while cookies_string.contains(&format!("filters{filters_number_to_delete_from}=")) {
			// Remove that filters cookie
			response.remove_cookie(format!("filters{filters_number_to_delete_from}"));

			// Increment filters cookie number
			filters_number_to_delete_from += 1;
		}
	} else {
		// Remove unnumbered filters cookie
		response.remove_cookie("filters".to_string());

		// Starts at one to deal with the first numbered subscription cookie and onwards
		let mut filters_number_to_delete_from = 1;

		// While filtersNUMBER= is in the string of cookies add a response removing that cookie
		while cookies_string.contains(&format!("filters{filters_number_to_delete_from}=")) {
			// Remove that sfilters cookie
			response.remove_cookie(format!("filters{filters_number_to_delete_from}"));

			// Increment filters cookie number
			filters_number_to_delete_from += 1;
		}
	}

	response
}

/// Set cookies using response "Set-Cookie" header
pub async fn restore(req: Request<Body>) -> Result<Response<Body>, String> {
	Ok(set_cookies_method(req, true))
}

pub async fn update(req: Request<Body>) -> Result<Response<Body>, String> {
	Ok(set_cookies_method(req, false))
}

/// Bulk-import subscriptions pasted from Reddit. Accepts the JSON from
/// reddit.com/subreddits/mine.json, the GDPR-export subscribed_subreddits.csv,
/// or a plain newline/comma-separated list. POST-only so the list never
/// appears in URLs, access logs, or browser history; the result is stored
/// exclusively in this browser's subscription cookies.
pub async fn import_subscriptions(req: Request<Body>) -> Result<Response<Body>, String> {
	let (parts, body) = req.into_parts();

	let body_bytes = hyper::body::to_bytes(body).await.map_err(|e| format!("Failed to read request body: {e}"))?;
	if body_bytes.len() > 1024 * 1024 {
		return Err("Request body too large".to_string());
	}

	let import_data = form_urlencoded::parse(&body_bytes)
		.find(|(key, _)| key == "import_data")
		.map(|(_, value)| value.into_owned())
		.unwrap_or_default();

	// Existing cookie header, needed to clean up stale numbered cookies
	let cookies_string = parts.headers.get("cookie").map(|hv| hv.to_str().unwrap_or("").to_string()).unwrap_or_default();

	// Existing subscriptions from this browser's cookies
	let req = Request::from_parts(parts, Body::empty());
	let mut sub_list = Preferences::new(&req).subscriptions;

	// Merge (case-insensitive dedupe), then sort like the subscribe endpoint
	for name in parse_subreddit_names(&import_data) {
		if !sub_list.iter().any(|s| s.to_lowercase() == name.to_lowercase()) {
			sub_list.push(name);
		}
	}
	sub_list.sort_by_key(|a| a.to_lowercase());

	let mut response = redirect("/settings");

	let mut subscriptions_number_to_delete_from = 0;
	for (subscriptions_number, list) in join_until_size_limit(&sub_list).into_iter().enumerate() {
		let subscriptions_cookie = if subscriptions_number == 0 {
			"subscriptions".to_string()
		} else {
			format!("subscriptions{subscriptions_number}")
		};

		response.insert_cookie(
			Cookie::build((subscriptions_cookie, list))
				.path("/")
				.http_only(true)
				.expires(OffsetDateTime::now_utc() + Duration::weeks(52))
				.into(),
		);

		subscriptions_number_to_delete_from += 1;
	}

	// Remove any leftover numbered subscription cookies beyond what we just set
	while cookies_string.contains(&format!("subscriptions{subscriptions_number_to_delete_from}=")) {
		response.remove_cookie(format!("subscriptions{subscriptions_number_to_delete_from}"));
		subscriptions_number_to_delete_from += 1;
	}

	Ok(response)
}

/// Extract subreddit names from pasted import data (JSON listing, CSV, or a
/// plain list of names/URLs).
fn parse_subreddit_names(input: &str) -> Vec<String> {
	let trimmed = input.trim();
	let mut names = Vec::new();

	// reddit.com/subreddits/mine.json (or any Reddit listing JSON): collect
	// every "display_name" field
	if trimmed.starts_with('{') || trimmed.starts_with('[') {
		if let Ok(json) = serde_json::from_str::<serde_json::Value>(trimmed) {
			collect_display_names(&json, &mut names);
		}
	}

	// GDPR CSV or plain list: one name, r/name, or URL per token
	if names.is_empty() {
		for token in trimmed.split(|c: char| c == ',' || c.is_whitespace()) {
			let token = token.trim().trim_matches('"').trim_matches('\'');
			let token = token
				.trim_start_matches("https://")
				.trim_start_matches("http://")
				.trim_start_matches("www.")
				.trim_start_matches("old.")
				.trim_start_matches("reddit.com");
			let token = token.trim_matches('/');
			let token = token.strip_prefix("r/").unwrap_or(token);

			// Skip the GDPR CSV header line
			if token.is_empty() || token.eq_ignore_ascii_case("subreddit") {
				continue;
			}

			if token.len() <= 24 && token.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
				names.push(token.to_string());
			}
		}
	}

	names
}

fn collect_display_names(value: &serde_json::Value, names: &mut Vec<String>) {
	match value {
		serde_json::Value::Object(map) => {
			if let Some(serde_json::Value::String(name)) = map.get("display_name") {
				names.push(name.clone());
			}
			for v in map.values() {
				collect_display_names(v, names);
			}
		}
		serde_json::Value::Array(arr) => {
			for v in arr {
				collect_display_names(v, names);
			}
		}
		_ => {}
	}
}

pub async fn encoded_restore(req: Request<Body>) -> Result<Response<Body>, String> {
	let body = hyper::body::to_bytes(req.into_body())
		.await
		.map_err(|e| format!("Failed to get bytes from request body: {e}"))?;

	if body.len() > 1024 * 1024 {
		return Err("Request body too large".to_string());
	}

	let encoded_prefs = form_urlencoded::parse(&body)
		.find(|(key, _)| key == "encoded_prefs")
		.map(|(_, value)| value)
		.ok_or_else(|| "encoded_prefs parameter not found in request body".to_string())?;

	let bytes = base2048::decode(&encoded_prefs).ok_or_else(|| "Failed to decode base2048 encoded preferences".to_string())?;

	let out = timeout(std::time::Duration::from_secs(1), async { deflate_decompress(bytes) })
		.await
		.map_err(|e| format!("Failed to decompress bytes: {e}"))??;

	let mut prefs: Preferences = timeout(std::time::Duration::from_secs(1), async { bincode::deserialize(&out) })
		.await
		.map_err(|e| format!("Failed to deserialize preferences: {e}"))?
		.map_err(|e| format!("Failed to deserialize bytes into Preferences struct: {e}"))?;

	prefs.available_themes = vec![];

	let url = format!("/settings/restore/?{}", prefs.to_urlencoded()?);

	Ok(redirect(&url))
}
