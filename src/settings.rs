#![allow(clippy::cmp_owned)]

use std::collections::HashMap;

// CRATES
use crate::server::ResponseExt;
use crate::subreddit::join_until_size_limit;
use crate::utils::{deflate_decompress, redirect, template, Preferences};
use askama::Template;
use cookie::Cookie;
use futures_lite::StreamExt;
use hyper::{body::HttpBody, Body, Request, Response};
use time::{Duration, OffsetDateTime};
use tokio::time::timeout;
use url::form_urlencoded;

// STRUCTS
#[derive(Template)]
#[template(path = "settings.html")]
struct SettingsTemplate {
	prefs: Preferences,
	url: String,
	/// How many subreddits the last import read, shown after an import.
	imported: Option<usize>,
	/// Reddit's `after` cursor when the pasted mine.json page wasn't the last one.
	import_next: Option<String>,
	/// The last import was rejected for being over IMPORT_BODY_LIMIT.
	import_too_large: bool,
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
	let query = req.uri().query().unwrap_or_default();
	let query_param = |name: &str| form_urlencoded::parse(query.as_bytes()).find(|(key, _)| key == name).map(|(_, value)| value.into_owned());
	Ok(template(&SettingsTemplate {
		prefs: Preferences::new(&req),
		url,
		imported: query_param("imported").and_then(|n| n.parse().ok()),
		import_next: query_param("import_next").filter(|after| is_listing_cursor(after)),
		import_too_large: query_param("import_error").as_deref() == Some("too_large"),
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

	let Some(body_bytes) = read_body_limited(body, IMPORT_BODY_LIMIT).await? else {
		return Ok(redirect("/settings?import_error=too_large"));
	};

	let form_field = |name: &str| form_urlencoded::parse(&body_bytes).find(|(key, _)| key == name).map(|(_, value)| value.into_owned());
	let import_data = form_field("import_data").unwrap_or_default();
	// Set by importSubscriptions.js, which sends only the names and this cursor.
	let import_after = form_field("import_after").filter(|after| is_listing_cursor(after));

	// Existing cookie header, needed to clean up stale numbered cookies
	let cookies_string = parts.headers.get("cookie").map(|hv| hv.to_str().unwrap_or("").to_string()).unwrap_or_default();

	// Existing subscriptions from this browser's cookies
	let req = Request::from_parts(parts, Body::empty());
	let mut sub_list = Preferences::new(&req).subscriptions;

	// Merge (case-insensitive dedupe), then sort like the subscribe endpoint
	let names = parse_subreddit_names(&import_data);
	let imported = names.len();
	for name in names {
		if !sub_list.iter().any(|s| s.to_lowercase() == name.to_lowercase()) {
			sub_list.push(name);
		}
	}
	sub_list.sort_by_key(|a| a.to_lowercase());

	// mine.json is paged (25 by default, 100 at most). When this wasn't the last
	// page, send the cursor back so the settings page can link the next one.
	// Only the cursor goes in the URL, never the subreddit names.
	let next = listing_after(&import_data)
		.or(import_after)
		.map(|after| format!("&import_next={after}"))
		.unwrap_or_default();
	let mut response = redirect(&format!("/settings?imported={imported}{next}"));

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

/// Largest import upload accepted. With JavaScript the browser sends only the
/// names (a few KB); this leaves room for a full 100-subreddit mine.json page
/// posted without it.
const IMPORT_BODY_LIMIT: usize = 8 * 1024 * 1024;

/// Reads the whole body, or returns `None` as soon as it passes `limit` bytes,
/// so an oversized upload is never buffered in full.
async fn read_body_limited(mut body: Body, limit: usize) -> Result<Option<Vec<u8>>, String> {
	let mut buf = Vec::new();
	while let Some(chunk) = body.data().await {
		let chunk = chunk.map_err(|e| format!("Failed to read request body: {e}"))?;
		if buf.len() + chunk.len() > limit {
			return Ok(None);
		}
		buf.extend_from_slice(&chunk);
	}
	Ok(Some(buf))
}

/// Reddit's `data.after` cursor from a pasted listing JSON, if there is a next page.
fn listing_after(input: &str) -> Option<String> {
	let json: serde_json::Value = serde_json::from_str(input.trim()).ok()?;
	let after = json["data"]["after"].as_str()?;
	is_listing_cursor(after).then(|| after.to_string())
}

/// A subreddit listing cursor looks like `t5_2qh1i`. Anything else is ignored,
/// since it ends up in a link on the settings page.
fn is_listing_cursor(after: &str) -> bool {
	after
		.strip_prefix("t5_")
		.is_some_and(|id| !id.is_empty() && id.len() <= 16 && id.chars().all(|c| c.is_ascii_alphanumeric()))
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

#[cfg(test)]
mod import_tests {
	use super::{is_listing_cursor, listing_after, parse_subreddit_names};

	#[test]
	fn reads_names_and_next_page_cursor() {
		let page = r#"{"kind":"Listing","data":{"after":"t5_2qh1i","children":[{"data":{"display_name":"rust"}},{"data":{"display_name":"linux"}}]}}"#;
		assert_eq!(parse_subreddit_names(page), vec!["rust", "linux"]);
		assert_eq!(listing_after(page).as_deref(), Some("t5_2qh1i"));
	}

	#[test]
	fn last_page_and_non_json_have_no_cursor() {
		assert_eq!(listing_after(r#"{"data":{"after":null,"children":[]}}"#), None);
		assert_eq!(listing_after("r/rust r/linux"), None);
	}

	#[tokio::test]
	async fn body_reader_stops_past_the_limit() {
		use super::read_body_limited;
		use hyper::Body;
		assert_eq!(read_body_limited(Body::from(vec![b'a'; 10]), 10).await, Ok(Some(vec![b'a'; 10])));
		assert_eq!(read_body_limited(Body::from(vec![b'a'; 11]), 10).await, Ok(None));
	}

	#[test]
	fn only_subreddit_cursors_are_accepted() {
		assert!(is_listing_cursor("t5_2qh1i"));
		for bad in ["", "t5_", "t3_abc", "t5_abc\"><script>", "t5_abc&x=1", "t5_aaaaaaaaaaaaaaaaaaaa"] {
			assert!(!is_listing_cursor(bad), "{bad}");
		}
	}
}
