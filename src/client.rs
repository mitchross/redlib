use crate::dbg_msg;
use crate::oauth::{force_refresh_token, token_daemon, Oauth, OauthBackendImpl};
use crate::server::RequestExt;
use crate::utils::{format_url, Post};
use arc_swap::ArcSwap;
use cached::proc_macro::cached;
use futures_lite::future::block_on;
use hyper::{header, Body, Request as HyperRequest, Response as HyperResponse};
use log::{error, info, trace, warn};
use percent_encoding::{percent_encode, CONTROLS};
use serde_json::Value;
use std::result::Result;
use std::sync::atomic::Ordering;
use std::sync::atomic::{AtomicBool, AtomicU16, AtomicU64};
use std::sync::LazyLock;
use std::time::{SystemTime, UNIX_EPOCH};
use wreq::redirect::Policy;
use wreq::{header as wreq_header, Client as WreqClient, EmulationFactory, Method, Response as WreqResponse};
use wreq_util::{Emulation, EmulationOS, EmulationOption};

const REDDIT_URL_BASE: &str = "https://oauth.reddit.com";
const REDDIT_URL_BASE_HOST: &str = "oauth.reddit.com";

const REDDIT_SHORT_URL_BASE: &str = "https://redd.it";
const REDDIT_SHORT_URL_BASE_HOST: &str = "redd.it";

const ALTERNATIVE_REDDIT_URL_BASE: &str = "https://www.reddit.com";
const ALTERNATIVE_REDDIT_URL_BASE_HOST: &str = "www.reddit.com";

pub static CLIENT: LazyLock<WreqClient> = LazyLock::new(build_client);

pub static OAUTH_CLIENT: LazyLock<ArcSwap<Oauth>> = LazyLock::new(|| {
	let client = block_on(Oauth::new());
	tokio::spawn(token_daemon());
	ArcSwap::new(client.into())
});

pub static OAUTH_RATELIMIT_REMAINING: AtomicU16 = AtomicU16::new(99);

pub static OAUTH_IS_ROLLING_OVER: AtomicBool = AtomicBool::new(false);

/// Unix seconds of the last token refresh triggered by a blocked (403) or
/// unauthorized (401) response.
static LAST_TRIGGERED_REFRESH: AtomicU64 = AtomicU64::new(0);

/// Minimum gap between 401/403-triggered refreshes, so a lasting block can't
/// hammer the token endpoint. Rate-limit rollovers deliberately skip this.
const TRIGGERED_REFRESH_COOLDOWN_SECS: u64 = 60;

const URL_PAIRS: [(&str, &str); 2] = [
	(ALTERNATIVE_REDDIT_URL_BASE, ALTERNATIVE_REDDIT_URL_BASE_HOST),
	(REDDIT_SHORT_URL_BASE, REDDIT_SHORT_URL_BASE_HOST),
];

pub fn build_client() -> WreqClient {
	// Keeping this list short to aid in privacy.
	// The more emulations, the more unique a fingerprint each instance has.
	// But some emulations should increase evasiveness.
	let emulation = [Emulation::Chrome145, Emulation::Firefox147];
	let emulation_os = [EmulationOS::Android, EmulationOS::Windows];

	let rand = fastrand::usize(..);
	let emulation = EmulationOption::builder()
		.emulation(emulation[rand % emulation.len()])
		.emulation_os(emulation_os[rand % emulation_os.len()])
		.build()
		.emulation();

	info!("Building Wreq client with random emulation {:?}", emulation);
	WreqClient::builder()
		.emulation(emulation)
		.redirect(Policy::none())
		.build()
		.expect("Should always be able to build a client")
}

/// Gets the canonical path for a resource on Reddit. This is accomplished by
/// making a `HEAD` request to Reddit at the path given in `path`.
///
/// This function returns `Ok(Some(path))`, where `path`'s value is identical
/// to that of the value of the argument `path`, if Reddit responds to our
/// `HEAD` request with a 2xx-family HTTP code. It will also return an
/// `Ok(Some(String))` if Reddit responds to our `HEAD` request with a
/// `Location` header in the response, and the HTTP code is in the 3xx-family;
/// the `String` will contain the path as reported in `Location`. The return
/// value is `Ok(None)` if Reddit responded with a 3xx, but did not provide a
/// `Location` header. An `Err(String)` is returned if Reddit responds with a
/// 429, or if we were unable to decode the value in the `Location` header.
#[cached(size = 1024, time = 600, result = true)]
pub async fn canonical_path(path: String, tries: i8) -> Result<Option<String>, String> {
	if tries == 0 {
		return Ok(None);
	}

	// for each URL pair, try the HEAD request
	let res = {
		// for url base and host in URL_PAIRS, try reddit_short_head(path.clone(), true, url_base, url_base_host) and if it succeeds, set res. else, res = None
		let mut res = None;
		for (url_base, url_base_host) in URL_PAIRS {
			res = reddit_short_head(path.clone(), true, url_base, url_base_host).await.ok();
			if let Some(res) = &res {
				if !res.status().is_client_error() {
					break;
				}
			}
		}
		res
	};

	let res = res.ok_or_else(|| "Unable to make HEAD request to Reddit.".to_string())?;
	let status = res.status().as_u16();
	let policy_error = res.headers().get(wreq_header::RETRY_AFTER).is_some();

	match status {
		// If Reddit responds with a 2xx, then the path is already canonical.
		200..=299 => Ok(Some(path)),

		// If Reddit responds with a 301, then the path is redirected.
		301 => match res.headers().get(wreq_header::LOCATION) {
			Some(val) => {
				let Ok(original) = val.to_str() else {
					return Err("Unable to decode Location header.".to_string());
				};

				// We need to strip the .json suffix from the original path.
				// In addition, we want to remove share parameters.
				// Cut it off here instead of letting it propagate all the way
				// to main.rs
				let stripped_uri = original.strip_suffix(".json").unwrap_or(original).split('?').next().unwrap_or_default();

				// The reason why we now have to format_url, is because the new OAuth
				// endpoints seem to return full paths, instead of relative paths.
				// So we need to strip the .json suffix from the original path, and
				// also remove all Reddit domain parts with format_url.
				// Otherwise, it will literally redirect to Reddit.com.
				let uri = format_url(stripped_uri);

				// Decrement tries and try again. Boxed because the future is recursive.
				Box::pin(canonical_path(uri, tries - 1)).await
			}
			None => Ok(None),
		},

		// If Reddit responds with anything other than 3xx (except for the 2xx and 301
		// as above), return a None.
		300..=399 => Ok(None),

		// Rate limiting
		429 => Err("Too many requests.".to_string()),

		// Special condition rate limiting - https://github.com/redlib-org/redlib/issues/229
		403 if policy_error => Err("Too many requests.".to_string()),

		_ => Ok(
			res
				.headers()
				.get(wreq_header::LOCATION)
				.map(|val| percent_encode(val.as_bytes(), CONTROLS).to_string().trim_start_matches(REDDIT_URL_BASE).to_string()),
		),
	}
}

pub async fn proxy(req: HyperRequest<Body>, format: &str) -> Result<HyperResponse<Body>, String> {
	let mut url = format!("{format}?{}", req.uri().query().unwrap_or_default());

	// For each parameter in request
	for (name, value) in &req.params() {
		// Fill the parameter value in the url
		url = url.replace(&format!("{{{name}}}"), value);
	}

	// First parameter is target URL (mandatory).
	let wreq_uri = wreq::Uri::try_from(url).map_err(|_| "Couldn't parse URL".to_string())?;

	let mut builder = CLIENT.get(wreq_uri);

	// Copy useful headers from original request
	for &key in &["Range", "If-Modified-Since", "Cache-Control"] {
		if let Some(value) = req.headers().get(key) {
			builder = builder.header(key, value.as_bytes());
		}
	}

	// Add User-Agent header of the currently spoofed device
	{
		let client = OAUTH_CLIENT.load_full();
		builder = builder.header("User-Agent", client.user_agent());
	}

	// This is needed or Reddit will redirect us to a /media landing page that just renders the image.
	builder = builder.header(wreq_header::ACCEPT, "*/*");

	builder
		.send()
		.await
		.map(|mut res| {
			let headers = res.headers_mut();

			let mut rm = |key: &str| headers.remove(key);

			rm("access-control-expose-headers");
			rm("server");
			rm("vary");
			rm("etag");
			rm("x-cdn");
			rm("x-cdn-client-region");
			rm("x-cdn-name");
			rm("x-cdn-server-region");
			rm("x-reddit-cdn");
			rm("x-reddit-video-features");
			rm("Nel");
			rm("Report-To");

			res.into_hyper_response()
		})
		.map_err(|e| e.to_string())
}

/// Makes a GET request to Reddit at `path`. By default, this will honor HTTP
/// 3xx codes Reddit returns and will automatically redirect.
async fn reddit_get(path: String, quarantine: bool) -> Result<WreqResponse, String> {
	request(&Method::GET, path, true, quarantine, REDDIT_URL_BASE, REDDIT_URL_BASE_HOST).await
}

/// Makes a HEAD request to Reddit at `path, using the short URL base. This will not follow redirects.
async fn reddit_short_head(path: String, quarantine: bool, base_path: &'static str, host: &'static str) -> Result<WreqResponse, String> {
	request(&Method::HEAD, path, false, quarantine, base_path, host).await
}

/// Opts in to quarantined and gated subreddits.
const QUARANTINE_COOKIE: &str = "_options=%7B%22pref_quarantine_optin%22%3A%20true%2C%20%22pref_gated_sr_optin%22%3A%20true%7D";

/// Most redirects `request` follows for one call, so a redirect loop errors out instead of spinning forever.
const MAX_REDIRECTS: u8 = 5;

/// Makes a request to Reddit. If `redirect` is `true`, follows the URL that
/// Reddit provides in the Location HTTP header, up to `MAX_REDIRECTS` times.
async fn request(method: &'static Method, mut path: String, redirect: bool, quarantine: bool, base_path: &'static str, host: &'static str) -> Result<WreqResponse, String> {
	let mut redirects = 0;

	loop {
		let response = send(method, &path, quarantine, base_path, host).await?;

		// Reddit may respond with a 3xx. Decide whether or not to
		// redirect based on caller params.
		if !(redirect && response.status().is_redirection()) {
			return Ok(response);
		}
		if redirects == MAX_REDIRECTS {
			return Err("Reddit redirected too many times".to_string());
		}
		redirects += 1;

		let location_header = response.headers().get(wreq::header::LOCATION);
		if location_header.and_then(|h| h.to_str().ok()) == Some(ALTERNATIVE_REDDIT_URL_BASE) {
			return Err("Reddit response was invalid".to_string());
		}
		path = location_header
			.map(|val| {
				// We need to make adjustments to the URI
				// we get back from Reddit. Namely, we
				// must:
				//
				//     1. Remove the authority (e.g.
				//     https://www.reddit.com) that may be
				//     present, so that we follow the
				//     path (and query parameters) as
				//     required.
				//
				//     2. Percent-encode the path.
				let new_path = percent_encode(val.as_bytes(), CONTROLS)
					.to_string()
					.trim_start_matches(REDDIT_URL_BASE)
					.trim_start_matches(ALTERNATIVE_REDDIT_URL_BASE)
					.to_string();
				format!("{new_path}{}raw_json=1", if new_path.contains('?') { "&" } else { "?" })
			})
			.unwrap_or_default();
	}
}

/// Sends a single request to Reddit at `path`, without following redirects.
async fn send(method: &'static Method, path: &str, quarantine: bool, base_path: &str, host: &str) -> Result<WreqResponse, String> {
	// Build Reddit URL from path.
	let url = format!("{base_path}{path}");
	let cookie = if quarantine { QUARANTINE_COOKIE } else { "" };

	// Borrow the headers instead of copying them; the builder copies each one in.
	let client = OAUTH_CLIENT.load_full();
	let mut headers: Vec<(&str, &str)> = vec![("Host", host), ("Cookie", cookie)];
	headers.extend(client.headers_map.iter().map(|(key, value)| (key.as_str(), value.as_str())));

	// shuffle headers: https://github.com/redlib-org/redlib/issues/324
	fastrand::shuffle(&mut headers);

	let mut builder = CLIENT.request(method.clone(), &url);
	for (key, value) in headers {
		builder = builder.header(key, value);
	}

	builder.send().await.map_err(|e| {
		dbg_msg!("{method} {REDDIT_URL_BASE}{path}: {}", e);
		e.to_string()
	})
}

/// Make a request to a Reddit API and parse the JSON response
#[cached(size = 100, time = 30, result = true)]
pub async fn json(path: String, quarantine: bool) -> Result<Value, String> {
	// Closure to quickly build errors
	let err = |msg: &str, e: String, path: String| -> Result<Value, String> {
		// eprintln!("{} - {}: {}", url, msg, e);
		Err(format!("{msg}: {e} | {path}"))
	};

	// First, handle rolling over the OAUTH_CLIENT if need be.
	let current_rate_limit = OAUTH_RATELIMIT_REMAINING.load(Ordering::SeqCst);
	let is_rolling_over = OAUTH_IS_ROLLING_OVER.load(Ordering::SeqCst);
	// No cooldown: switching to a fresh token is how redlib stays under Reddit's
	// per-token rate limit, and under load that is needed more than once a minute
	// (a cooldown here caused 2.2.7's rate-limit outage). force_refresh_token
	// already lets only one refresh run at a time.
	if current_rate_limit < 10 && !is_rolling_over {
		warn!("Rate limit {current_rate_limit} is low; rolling over to a new token");
		tokio::spawn(force_refresh_token());
	}
	// Stop at 0. `fetch_sub` would wrap to 65535 and hide the low-limit check
	// above until Reddit's next rate-limit header resets the count.
	// (A plain CAS loop: `fetch_update` is deprecated on newer Rust, and its
	// replacement `try_update` isn't available at our MSRV.)
	let mut remaining = OAUTH_RATELIMIT_REMAINING.load(Ordering::SeqCst);
	while remaining > 0 {
		match OAUTH_RATELIMIT_REMAINING.compare_exchange_weak(remaining, remaining - 1, Ordering::SeqCst, Ordering::SeqCst) {
			Ok(_) => break,
			Err(actual) => remaining = actual,
		}
	}

	// Fetch the url...
	match reddit_get(path.clone(), quarantine).await {
		Ok(response) => {
			let status = response.status();

			let reset: Option<String> = if let (Some(remaining), Some(reset), Some(used)) = (
				response.headers().get("x-ratelimit-remaining").and_then(|val| val.to_str().ok().map(|s| s.to_string())),
				response.headers().get("x-ratelimit-reset").and_then(|val| val.to_str().ok().map(|s| s.to_string())),
				response.headers().get("x-ratelimit-used").and_then(|val| val.to_str().ok().map(|s| s.to_string())),
			) {
				trace!(
					"Ratelimit remaining: Header says {remaining}, we have {current_rate_limit}. Resets in {reset}. Rollover: {}. Ratelimit used: {used}",
					if is_rolling_over { "yes" } else { "no" },
				);

				// If can parse remaining as a float, round to a u16 and save
				if let Ok(val) = remaining.parse::<f32>() {
					OAUTH_RATELIMIT_REMAINING.store(val.round() as u16, Ordering::SeqCst);
				}

				Some(reset)
			} else {
				None
			};

			// Read the whole body straight from wreq; no detour through a hyper Response.
			match response.bytes().await {
				Ok(body) => {
					if body.is_empty() {
						// Rate limited: roll over to a fresh token right away (see the
						// low-rate-limit check above for why there's no cooldown).
						tokio::spawn(force_refresh_token());
						return match reset {
							Some(val) => Err(format!(
								"Reddit rate limit exceeded. Try refreshing in a few seconds.\
								 Rate limit will reset in: {val}"
							)),
							None => Err("Reddit rate limit exceeded".to_string()),
						};
					}

					// Parse the response from Reddit as JSON
					match serde_json::from_slice::<Value>(&body) {
						Ok(json) => {
							// If user is suspended
							if let Some(data) = json.get("data") {
								if let Some(is_suspended) = data.get("is_suspended").and_then(Value::as_bool) {
									if is_suspended {
										return Err("suspended".into());
									}
								}
							}

							// If Reddit returned an error
							if json["error"].is_i64() {
								// OAuth token has expired; http status 401
								if json["message"] == "Unauthorized" {
									if refresh_due("Reddit says the OAuth token is unauthorized") {
										force_refresh_token().await;
									}
									return Err("OAuth token has expired. Please refresh the page!".to_string());
								}

								// Handle quarantined
								if json["reason"] == "quarantined" {
									return Err("quarantined".into());
								}
								// Handle gated
								if json["reason"] == "gated" {
									return Err("gated".into());
								}
								// Handle private subs
								if json["reason"] == "private" {
									return Err("private".into());
								}
								// Handle banned subs
								if json["reason"] == "banned" {
									return Err("banned".into());
								}

								Err(format!("Reddit error {} \"{}\": {} | {path}", json["error"], json["reason"], json["message"]))
							} else {
								Ok(json)
							}
						}
						Err(e) => {
							error!("Got an invalid response from reddit {e}. Status code: {status}");
							// 429: this token's rate limit is spent, so roll over to a fresh one now.
							if status.as_u16() == 429 {
								tokio::spawn(force_refresh_token());
							}
							// Reddit answers a blocked token with an HTML 403; only a new token (device identity) clears it.
							if status.as_u16() == 403 && refresh_due("Reddit returned a non-JSON 403") {
								tokio::spawn(force_refresh_token());
							}
							if status.is_server_error() {
								Err("Reddit is having issues, check if there's an outage".to_string())
							} else {
								err("Failed to parse page JSON data", e.to_string(), path)
							}
						}
					}
				}
				Err(e) => err("Failed receiving body from Reddit", e.to_string(), path),
			}
		}
		Err(e) => err("Couldn't send request to Reddit", e, path),
	}
}

/// Whether a response-triggered token refresh may start now. Every trigger
/// shares one cooldown, so at most one such refresh starts per minute.
fn refresh_due(reason: &str) -> bool {
	let due = claim_refresh_slot(&LAST_TRIGGERED_REFRESH, unix_now(), TRIGGERED_REFRESH_COOLDOWN_SECS);
	if due {
		warn!("{reason}; refreshing the OAuth token");
	} else {
		trace!("{reason}; token refresh still on cooldown");
	}
	due
}

fn unix_now() -> u64 {
	SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

/// Returns true for at most one caller per `cooldown` seconds.
fn claim_refresh_slot(last: &AtomicU64, now: u64, cooldown: u64) -> bool {
	let prev = last.load(Ordering::SeqCst);
	now.saturating_sub(prev) >= cooldown && last.compare_exchange(prev, now, Ordering::SeqCst, Ordering::SeqCst).is_ok()
}

async fn self_check(sub: &str) -> Result<(), String> {
	let query = format!("/r/{sub}/hot.json?&raw_json=1");

	match Post::fetch(&query, true).await {
		Ok(_) => Ok(()),
		Err(e) => Err(e),
	}
}

pub async fn rate_limit_check() -> Result<(), String> {
	// First, test the Oauth client: we can perform a rate limit check if the OAuth backend is MobileSpoof; if GenericWeb, we skip the check.
	if matches!(OAUTH_CLIENT.load().backend, OauthBackendImpl::GenericWeb(_)) {
		warn!("[⚠️] Cannot perform rate limit check, running as GenericWeb. Skipping check.");
		return Ok(());
	}

	// First, check a subreddit.
	self_check("reddit").await?;
	// This will reduce the rate limit to 99. Assert this check.
	if OAUTH_RATELIMIT_REMAINING.load(Ordering::SeqCst) != 99 {
		return Err(format!("Rate limit check 1 failed: expected 99, got {}", OAUTH_RATELIMIT_REMAINING.load(Ordering::SeqCst)));
	}
	// Now, we switch out the OAuth client.
	// This checks for the IP rate limit association.
	force_refresh_token().await;
	// Now, check a new sub to break cache.
	self_check("rust").await?;
	// Again, assert the rate limit check.
	if OAUTH_RATELIMIT_REMAINING.load(Ordering::SeqCst) != 99 {
		return Err(format!("Rate limit check 2 failed: expected 99, got {}", OAUTH_RATELIMIT_REMAINING.load(Ordering::SeqCst)));
	}

	Ok(())
}

trait IntoHyperResponse {
	fn into_hyper_response(self) -> HyperResponse<Body>;
}

impl IntoHyperResponse for WreqResponse {
	fn into_hyper_response(self) -> HyperResponse<Body> {
		let status = self.status();
		let version = self.version();

		let mut builder = HyperResponse::builder().status(status.as_u16()).version(match version {
			wreq::Version::HTTP_09 => hyper::Version::HTTP_09,
			wreq::Version::HTTP_10 => hyper::Version::HTTP_10,
			wreq::Version::HTTP_11 => hyper::Version::HTTP_11,
			wreq::Version::HTTP_2 => hyper::Version::HTTP_2,
			wreq::Version::HTTP_3 => hyper::Version::HTTP_3,
			_ => hyper::Version::HTTP_11,
		});

		for (name, value) in self.headers() {
			builder = builder.header(
				header::HeaderName::from_bytes(name.as_str().as_bytes()).unwrap(),
				header::HeaderValue::from_bytes(value.as_bytes()).unwrap(),
			);
		}

		builder.body(Body::wrap_stream(self.bytes_stream())).unwrap()
	}
}

#[cfg(test)]
mod tests {
	#[test]
	fn triggered_refresh_is_rate_limited() {
		use super::claim_refresh_slot;
		use std::sync::atomic::AtomicU64;
		let last = AtomicU64::new(0);
		assert!(claim_refresh_slot(&last, 1_000, 60));
		assert!(!claim_refresh_slot(&last, 1_030, 60));
		assert!(!claim_refresh_slot(&last, 1_059, 60));
		assert!(claim_refresh_slot(&last, 1_060, 60));
		assert!(!claim_refresh_slot(&last, 1_061, 60));
	}

	use super::*;
	use {crate::config::get_setting, sealed_test::prelude::*};

	const POPULAR_URL: &str = "/r/popular/hot.json?&raw_json=1&geo_filter=GLOBAL";

	#[tokio::test(flavor = "multi_thread")]
	#[ignore] // Reddit blocks GitHub Actions IPs
	async fn test_rate_limit_check() {
		rate_limit_check().await.unwrap();
	}

	#[test]
	#[ignore] // Reddit blocks GitHub Actions IPs
	#[sealed_test(env = [("REDLIB_DEFAULT_SUBSCRIPTIONS", "rust")])]
	fn test_default_subscriptions() {
		tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap().block_on(async {
			let subscriptions = get_setting("REDLIB_DEFAULT_SUBSCRIPTIONS");
			assert!(subscriptions.is_some());

			// check rate limit
			rate_limit_check().await.unwrap();
		});
	}

	#[tokio::test(flavor = "multi_thread")]
	#[ignore] // Reddit blocks GitHub Actions IPs
	async fn test_localization_popular() {
		let val = json(POPULAR_URL.to_string(), false).await.unwrap();
		assert_eq!("GLOBAL", val["data"]["geo_filter"].as_str().unwrap());
	}

	#[tokio::test(flavor = "multi_thread")]
	#[ignore] // Reddit blocks GitHub Actions IPs
	async fn test_obfuscated_share_link() {
		let share_link = "/r/rust/s/kPgq8WNHRK".into();
		// Correct link without share parameters
		let canonical_link = "/r/rust/comments/18t5968/why_use_tuple_struct_over_standard_struct/kfbqlbc/".into();
		assert_eq!(canonical_path(share_link, 3).await, Ok(Some(canonical_link)));
	}

	#[tokio::test(flavor = "multi_thread")]
	#[ignore] // Reddit blocks GitHub Actions IPs
	async fn test_private_sub() {
		let link = json("/r/suicide/about.json?raw_json=1".into(), true).await;
		assert!(link.is_err());
		assert_eq!(link, Err("private".into()));
	}

	#[tokio::test(flavor = "multi_thread")]
	#[ignore] // Reddit blocks GitHub Actions IPs
	async fn test_banned_sub() {
		let link = json("/r/aaa/about.json?raw_json=1".into(), true).await;
		assert!(link.is_err());
		assert_eq!(link, Err("banned".into()));
	}

	#[tokio::test(flavor = "multi_thread")]
	#[ignore] // Reddit blocks GitHub Actions IPs
	async fn test_gated_sub() {
		// quarantine to false to specifically catch when we _don't_ catch it
		let link = json("/r/drugs/about.json?raw_json=1".into(), false).await;
		assert!(link.is_err());
		assert_eq!(link, Err("gated".into()));
	}
}
