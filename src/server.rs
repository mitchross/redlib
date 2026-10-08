#![allow(dead_code)]
#![allow(clippy::cmp_owned)]

use brotli::enc::{BrotliCompress, BrotliEncoderParams};
use cached::proc_macro::cached;
use cookie::Cookie;
use futures_lite::{future::Boxed, Future, FutureExt};
use hyper::{
	body,
	body::HttpBody,
	header,
	service::{make_service_fn, service_fn},
	HeaderMap,
};
use hyper::{Body, Method, Request, Response, Server as HyperServer};
use libflate::gzip;
use route_recognizer::{Params, Router};
use std::{
	cmp::Ordering,
	fmt::Display,
	io,
	pin::Pin,
	result::Result,
	str::{from_utf8, Split},
	string::ToString,
	sync::Arc,
};
use time::OffsetDateTime;

use crate::{analytics::ANALYTICS, config, dbg_msg, utils::register_active_user};

const BANNED_USER_AGENTS: &[&str] = &[
	"AI2Bot",
	"Ai2Bot-Dolma",
	"Amazonbot",
	"Andibot",
	"Applebot",
	"Applebot-Extended",
	"Awario",
	"Brightbot 1.0",
	"Bytespider",
	"CCBot",
	"ChatGPT-User",
	"Claude-SearchBot",
	"Claude-User",
	"Claude-Web",
	"ClaudeBot",
	"Cotoyogi",
	"Crawlspace",
	"Datenbank Crawler",
	"Devin",
	"Diffbot",
	"DuckAssistBot",
	"Echobot Bot",
	"EchoboxBot",
	"FacebookBot",
	"Factset_spyderbot",
	"FirecrawlAgent",
	"FriendlyCrawler",
	"GPTBot",
	"Google-CloudVertexBot",
	"Google-Extended",
	"GoogleOther",
	"GoogleOther-Image",
	"GoogleOther-Video",
	"ICC-Crawler",
	"ISSCyberRiskCrawler",
	"ImagesiftBot",
	"Kangaroo Bot",
	"Meta-ExternalAgent",
	"Meta-ExternalFetcher",
	"MistralAI-User",
	"MistralAI-User/1.0",
	"MyCentralAIScraperBot",
	"NovaAct",
	"OAI-SearchBot",
	"Operator",
	"PanguBot",
	"Panscient",
	"Perplexity-User",
	"PerplexityBot",
	"PetalBot",
	"PhindBot",
	"Poseidon Research Crawler",
	"QualifiedBot",
	"QuillBot",
	"SBIntuitionsBot",
	"Scrapy",
	"SemrushBot",
	"SemrushBot-BA",
	"SemrushBot-CT",
	"SemrushBot-OCOB",
	"SemrushBot-SI",
	"SemrushBot-SWA",
	"Sidetrade indexer bot",
	"TikTokSpider",
	"Timpibot",
	"VelenPublicWebCrawler",
	"WARDBot",
	"Webzio-Extended",
	"YandexAdditional",
	"YandexAdditionalBot",
	"YouBot",
	"aiHitBot",
	"anthropic-ai",
	"bedrockbot",
	"cohere-ai",
	"cohere-training-data-crawler",
	"facebookexternalhit",
	"iaskspider/2.0",
	"img2dataset",
	"meta-externalagent",
	"meta-externalfetcher",
	"omgili",
	"omgilibot",
	"panscient.com",
	"quillbot.com",
	"wpbot",
];

type BoxResponse = Pin<Box<dyn Future<Output = Result<Response<Body>, String>> + Send>>;
type Handler = fn(Request<Body>) -> BoxResponse;

/// Compressors for the response Body, in ascending order of preference.
#[derive(Copy, Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
enum CompressionType {
	Passthrough,
	Gzip,
	Brotli,
}

/// All browsers support gzip, so if we are given `Accept-Encoding: *`, deliver
/// gzipped-content.
///
/// Brotli would be nice universally, but Safari (iOS, iPhone, macOS) reportedly
/// doesn't support it yet.
const DEFAULT_COMPRESSOR: CompressionType = CompressionType::Gzip;

/// Brotli quality (0–11) for response bodies. See `compress_body`.
const BROTLI_QUALITY: i32 = 5;

impl CompressionType {
	/// The content coding token, as used in `Content-Encoding`.
	const fn as_str(self) -> &'static str {
		match self {
			Self::Gzip => "gzip",
			Self::Brotli => "br",
			Self::Passthrough => "",
		}
	}

	/// Returns a `CompressionType` given a content coding
	/// in [RFC 7231](https://datatracker.ietf.org/doc/html/rfc7231#section-5.3.4)
	/// format.
	fn parse(s: &str) -> Option<Self> {
		let c = match s {
			// Compressors we support.
			"gzip" => Self::Gzip,
			"br" => Self::Brotli,

			// The wildcard means that we can choose whatever
			// compression we prefer. In this case, use the
			// default.
			"*" => DEFAULT_COMPRESSOR,

			// Compressor not supported.
			_ => return None,
		};

		Some(c)
	}
}

impl Display for CompressionType {
	fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
		f.write_str(self.as_str())
	}
}

pub struct Route<'a> {
	router: &'a mut Router<Handler>,
	path: String,
}

pub struct Server {
	pub default_headers: HeaderMap,
	router: Router<Handler>,
}

#[macro_export]
macro_rules! headers(
	{ $($key:expr => $value:expr),+ } => {
		{
			let mut m = hyper::HeaderMap::new();
			$(
				if let Ok(val) = hyper::header::HeaderValue::from_str($value) {
					m.insert($key, val);
				}
			)+
			m
		}
	 };
);

pub trait RequestExt {
	fn params(&self) -> Params;
	fn param(&self, name: &str) -> Option<String>;
	fn set_params(&mut self, params: Params) -> Option<Params>;
	fn cookies(&self) -> Vec<Cookie<'_>>;
	fn cookie(&self, name: &str) -> Option<Cookie<'_>>;
}

pub trait ResponseExt {
	fn cookies(&self) -> Vec<Cookie<'_>>;
	fn insert_cookie(&mut self, cookie: Cookie<'_>);
	fn remove_cookie(&mut self, name: String);
}

/// Parses every cookie in the `Cookie` header(s). The cookies borrow from
/// `headers`, so no names or values are copied. Malformed pairs are skipped.
fn parse_cookies(headers: &HeaderMap) -> Vec<Cookie<'_>> {
	headers
		.get_all(header::COOKIE)
		.iter()
		.filter_map(|hdr| hdr.to_str().ok())
		.flat_map(Cookie::split_parse)
		.filter_map(Result::ok)
		.collect()
}

impl RequestExt for Request<Body> {
	fn params(&self) -> Params {
		self.extensions().get::<Params>().cloned().unwrap_or_default()
	}

	fn param(&self, name: &str) -> Option<String> {
		// Look the value up in place instead of cloning every route param first.
		self.extensions().get::<Params>()?.find(name).map(ToOwned::to_owned)
	}

	fn set_params(&mut self, params: Params) -> Option<Params> {
		self.extensions_mut().insert(params)
	}

	fn cookies(&self) -> Vec<Cookie<'_>> {
		parse_cookies(self.headers())
	}

	fn cookie(&self, name: &str) -> Option<Cookie<'_>> {
		self.cookies().into_iter().find(|c| c.name() == name)
	}
}

impl ResponseExt for Response<Body> {
	fn cookies(&self) -> Vec<Cookie<'_>> {
		parse_cookies(self.headers())
	}

	fn insert_cookie(&mut self, cookie: Cookie<'_>) {
		if let Ok(val) = header::HeaderValue::from_str(&cookie.to_string()) {
			self.headers_mut().append("Set-Cookie", val);
		}
	}

	fn remove_cookie(&mut self, name: String) {
		let removal_cookie = Cookie::build(name).path("/").http_only(true).expires(OffsetDateTime::now_utc());
		if let Ok(val) = header::HeaderValue::from_str(&removal_cookie.to_string()) {
			self.headers_mut().append("Set-Cookie", val);
		}
	}
}

impl Route<'_> {
	fn method(&mut self, method: &Method, dest: Handler) -> &mut Self {
		self.router.add(&format!("/{}{}", method.as_str(), self.path), dest);
		self
	}

	/// Add an endpoint for `GET` requests
	pub fn get(&mut self, dest: Handler) -> &mut Self {
		self.method(&Method::GET, dest)
	}

	/// Add an endpoint for `POST` requests
	pub fn post(&mut self, dest: Handler) -> &mut Self {
		self.method(&Method::POST, dest)
	}
}

impl Default for Server {
	fn default() -> Self {
		Self::new()
	}
}

impl Server {
	pub fn new() -> Self {
		Self {
			default_headers: HeaderMap::new(),
			router: Router::new(),
		}
	}

	pub fn at(&mut self, path: &str) -> Route<'_> {
		Route {
			path: path.to_owned(),
			router: &mut self.router,
		}
	}

	pub fn listen(self, addr: &str) -> Boxed<Result<(), hyper::Error>> {
		// Shared by every connection. Cloning the `Router` per connection would
		// deep-copy the whole route table on each TCP accept.
		let state = Arc::new(ServerState {
			router: self.router,
			default_headers: self.default_headers,
			// CONFIG is immutable after startup, so read these once rather than per request.
			block_bots: config::get_setting("REDLIB_ROBOTS_DISABLE_INDEXING").is_some_and(|val| val == "on"),
			real_ip_header: real_ip_header(),
		});

		let make_svc = make_service_fn(move |_conn| {
			let state = Arc::clone(&state);

			// This is the `Service` that will handle the connection.
			async move { Ok::<_, String>(service_fn(move |req| handle(req, Arc::clone(&state)))) }
		});

		// Build SocketAddr from provided address
		let address = &addr.parse().unwrap_or_else(|_| panic!("Cannot parse {addr} as address (example format: 0.0.0.0:8080)"));

		// Bind server to address specified above. Gracefully shut down if CTRL+C is pressed
		let server = HyperServer::bind(address).serve(make_svc).with_graceful_shutdown(async {
			#[cfg(windows)]
			// Wait for the CTRL+C signal
			tokio::signal::ctrl_c().await.expect("Failed to install CTRL+C signal handler");

			#[cfg(unix)]
			{
				// Wait for CTRL+C or SIGTERM signals
				let mut signal_terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("Failed to install SIGTERM signal handler");
				tokio::select! {
					_ = tokio::signal::ctrl_c() => (),
					_ = signal_terminate.recv() => ()
				}
			}
		});

		server.boxed()
	}
}

/// Everything `handle` needs that is fixed once the server starts.
struct ServerState {
	router: Router<Handler>,
	default_headers: HeaderMap,
	block_bots: bool,
	real_ip_header: Option<header::HeaderName>,
}

/// Reads `REDLIB_REAL_IP_HEADER`. An empty or invalid value means "not set".
fn real_ip_header() -> Option<header::HeaderName> {
	let name = config::get_setting("REDLIB_REAL_IP_HEADER").filter(|name| !name.trim().is_empty())?;
	match header::HeaderName::from_bytes(name.trim().as_bytes()) {
		Ok(name) => Some(name),
		Err(_) => {
			log::warn!("Ignoring REDLIB_REAL_IP_HEADER: {name:?} is not a valid header name");
			None
		}
	}
}

/// Picks the visitor's IP from the request headers.
///
/// With `REDLIB_REAL_IP_HEADER` set (e.g. `CF-Connecting-IP` behind Cloudflare),
/// only that header is trusted. Otherwise this falls back to the first
/// `X-Forwarded-For` entry, then `X-Real-IP`. A client can forge both unless a
/// proxy in front overwrites them.
fn client_ip<'a>(headers: &'a HeaderMap, real_ip_header: Option<&header::HeaderName>) -> Option<&'a str> {
	let ip = match real_ip_header {
		Some(name) => headers.get(name).and_then(|v| v.to_str().ok()),
		None => headers
			.get("x-forwarded-for")
			.and_then(|v| v.to_str().ok())
			.and_then(|s| s.split(',').next())
			.or_else(|| headers.get("x-real-ip").and_then(|v| v.to_str().ok())),
	};
	ip.map(str::trim).filter(|ip| !ip.is_empty())
}

/// Route one request, then apply the default headers and compression.
async fn handle(mut req: Request<Body>, state: Arc<ServerState>) -> Result<Response<Body>, String> {
	let ServerState {
		router,
		default_headers,
		block_bots,
		real_ip_header,
	} = &*state;

	// Only Accept-Encoding is needed once `req` moves into the route handler,
	// so keep that one value (a refcount bump) rather than cloning every header.
	let accept_encoding = req.headers().get(header::ACCEPT_ENCODING).cloned();
	let accept_encoding = accept_encoding.as_ref();

	// Every borrow of `req` below (headers, ip, method) must end before `req`
	// is moved into the route handler. They all feed synchronous code that runs
	// first, so the move further down type-checks.
	let req_headers = req.headers();

	// Catch robots.txt-disrespecful bots who still identify themselves
	// Typically justified as "human triggered" actions.
	if *block_bots {
		let user_agent = req_headers.get(header::USER_AGENT).and_then(|v| v.to_str().ok()).unwrap_or_default();
		if BANNED_USER_AGENTS.iter().any(|banned| user_agent.contains(banned)) {
			return new_boilerplate(default_headers, accept_encoding, 403, Body::from("Forbidden")).await;
		}
	}

	// Track active users by IP. Requests with no client IP, such as kubelet
	// probes, aren't visitors and would otherwise all count as one "unknown" user.
	let ip = client_ip(req_headers, real_ip_header.as_ref());
	if let Some(ip) = ip {
		register_active_user(ip);
	}

	// Remove double slashes and decode encoded slashes
	let mut path = req.uri().path().replace("//", "/").replace("%2F", "/");

	// Remove trailing slashes
	if path != "/" && path.ends_with('/') {
		path.pop();
	}

	// Replace HEAD with GET for routing
	let (method, is_head) = match req.method() {
		&Method::HEAD => (&Method::GET, true),
		method => (method, false),
	};

	// Server-side analytics for HTML navigations only.
	// Checked first so the strings and the task are skipped when pageviews are off.
	if method == Method::GET && ANALYTICS.captures_pageviews() {
		let accept_html = req_headers.get("accept").and_then(|v| v.to_str().ok()).is_some_and(|v| v.contains("text/html"));

		if accept_html && !crate::analytics::sends_privacy_signal(req_headers) {
			let ua = req_headers.get("user-agent").and_then(|v| v.to_str().ok()).unwrap_or("").to_owned();
			let host = req_headers.get("host").and_then(|v| v.to_str().ok()).unwrap_or("").to_owned();
			let referrer = req_headers.get("referer").and_then(|v| v.to_str().ok()).unwrap_or("").to_owned();
			let ip_owned = ip.unwrap_or("unknown").to_owned();
			let path_for_event = path.clone();

			tokio::spawn(async move {
				ANALYTICS.capture_pageview(&path_for_event, &ua, &ip_owned, &host, &referrer).await;
			});
		}
	}

	// Match the visited path with an added route
	let found = match router.recognize(&format!("/{}{}", method.as_str(), path)) {
		Ok(found) => found,
		// If there was a routing error
		Err(e) => return new_boilerplate(default_headers, accept_encoding, 404, if is_head { Body::empty() } else { e.into() }).await,
	};

	// Run the route's function
	req.set_params(found.params().clone());
	let (result, stale_age) = crate::stale::scope((**found.handler())(req)).await;
	match result {
		Ok(mut res) => {
			res.headers_mut().extend(default_headers.clone());
			if let Some(age) = stale_age {
				// Built from a copy: say so, and keep shared caches from storing it.
				res.headers_mut().insert("X-Redlib-Stale-Age", header::HeaderValue::from(age));
				res.headers_mut().insert(header::CACHE_CONTROL, header::HeaderValue::from_static("no-store"));
			}
			if is_head {
				*res.body_mut() = Body::empty();
			} else {
				let _ = compress_response(accept_encoding, &mut res).await;
			}

			Ok(res)
		}
		Err(msg) => new_boilerplate(default_headers, accept_encoding, 500, if is_head { Body::empty() } else { Body::from(msg) }).await,
	}
}

/// Create a boilerplate Response for error conditions. This response will be
/// compressed if requested by client.
async fn new_boilerplate(default_headers: &HeaderMap, accept_encoding: Option<&header::HeaderValue>, status: u16, body: Body) -> Result<Response<Body>, String> {
	let mut res = Response::builder().status(status).body(body).map_err(|e| e.to_string())?;
	let _ = compress_response(accept_encoding, &mut res).await;

	res.headers_mut().extend(default_headers.clone());
	Ok(res)
}

/// Determines the desired compressor based on the Accept-Encoding header.
///
/// This function will honor the [q-value](https://developer.mozilla.org/en-US/docs/Glossary/Quality_values)
///  for each compressor. The q-value is an optional parameter, a decimal value
/// on \[0..1\], to order the compressors by preference. An Accept-Encoding value
/// with no q-values is also accepted.
///
/// Here are [examples](https://developer.mozilla.org/en-US/docs/Web/HTTP/Headers/Accept-Encoding#examples)
/// of valid Accept-Encoding headers.
///
/// ```http
/// Accept-Encoding: gzip
/// Accept-Encoding: gzip, compress, br
/// Accept-Encoding: br;q=1.0, gzip;q=0.8, *;q=0.1
/// ```
///
/// Deliberately not `#[cached]`: the input is client-controlled, so a cache
/// keyed on it grows without bound, and the parse is cheaper than a lookup.
fn determine_compressor(accept_encoding: &str) -> Option<CompressionType> {
	if accept_encoding.is_empty() {
		return None;
	};

	// Keep track of the compressor candidate based on both the client's
	// preference and our own. Concrete examples:
	//
	// 1. "Accept-Encoding: gzip, br" => assuming we like brotli more than
	//    gzip, and the browser supports brotli, we choose brotli
	//
	// 2. "Accept-Encoding: gzip;q=0.8, br;q=0.3" => the client has stated a
	//    preference for gzip over brotli, so we choose gzip
	//
	// To do this, we need to define a struct which contains the requested
	// requested compressor (abstracted as a CompressionType enum) and the
	// q-value. If no q-value is defined for the compressor, we assume one of
	// 1.0. We first compare compressor candidates by comparing q-values, and
	// then CompressionTypes. We keep track of whatever is the greatest per our
	// ordering.

	struct CompressorCandidate {
		alg: CompressionType,
		q: f64,
	}

	impl Ord for CompressorCandidate {
		fn cmp(&self, other: &Self) -> Ordering {
			// Compare q-values. Break ties with the
			// CompressionType values.

			match self.q.total_cmp(&other.q) {
				Ordering::Equal => self.alg.cmp(&other.alg),
				ord => ord,
			}
		}
	}

	impl PartialOrd for CompressorCandidate {
		fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
			Some(self.cmp(other))
		}
	}

	impl PartialEq for CompressorCandidate {
		fn eq(&self, other: &Self) -> bool {
			(self.q == other.q) && (self.alg == other.alg)
		}
	}

	impl Eq for CompressorCandidate {}

	// This is the current candidate.
	//
	// Assmume no candidate so far. We do this by assigning the sentinel value
	// of negative infinity to the q-value. If this value is negative infinity,
	// that means there was no viable compressor candidate.
	let mut cur_candidate = CompressorCandidate {
		alg: CompressionType::Passthrough,
		q: f64::NEG_INFINITY,
	};

	// This loop reads the requested compressors and keeps track of whichever
	// one has the highest priority per our heuristic.
	for val in accept_encoding.split(',') {
		let mut q: f64 = 1.0;

		// The compressor and q-value (if the latter is defined)
		// will be delimited by semicolons.
		let mut spl: Split<'_, char> = val.split(';');

		// Get the compressor. For example, in
		//   gzip;q=0.8
		// this grabs "gzip" in the string. It
		// will further validate the compressor against the
		// list of those we support. If it is not supported,
		// we move onto the next one.
		let compressor: CompressionType = match spl.next() {
			// CompressionType::parse will return the appropriate enum given
			// a string. For example, it will return CompressionType::Gzip
			// when given "gzip".
			Some(s) => match CompressionType::parse(s.trim()) {
				Some(candidate) => candidate,

				// We don't support the requested compression algorithm.
				None => continue,
			},

			// We should never get here, but I'm paranoid.
			None => continue,
		};

		// Get the q-value. This might not be defined, in which case assume
		// 1.0.
		if let Some(s) = spl.next() {
			if !(s.len() > 2 && s.starts_with("q=")) {
				// If the q-value is malformed, the header is malformed, so
				// abort.
				return None;
			}

			match s[2..].parse::<f64>() {
				Ok(val) => {
					if (0.0..=1.0).contains(&val) {
						q = val;
					} else {
						// If the value is outside [0..1], header is malformed.
						// Abort.
						return None;
					};
				}
				Err(_) => {
					// If this isn't a f64, then assume a malformed header
					// value and abort.
					return None;
				}
			}
		};

		// If new_candidate > cur_candidate, make new_candidate the new
		// cur_candidate. But do this safely! It is very possible that
		// someone gave us the string "NAN", which (&str).parse::<f64>
		// will happily translate to f64::NAN.
		let new_candidate = CompressorCandidate { alg: compressor, q };
		if let Some(ord) = new_candidate.partial_cmp(&cur_candidate) {
			if ord == Ordering::Greater {
				cur_candidate = new_candidate;
			}
		};
	}

	if cur_candidate.q == f64::NEG_INFINITY {
		None
	} else {
		Some(cur_candidate.alg)
	}
}

/// Compress the response body, if possible or desirable. The Body will be
/// compressed in place, and a new header Content-Encoding will be set
/// indicating the compression algorithm.
///
/// This function deems Body eligible compression if and only if the following
/// conditions are met:
///
/// 1. the HTTP client requests a compression encoding in the Content-Encoding
///    header (hence the need for `accept_encoding`);
///
/// 2. the content encoding corresponds to a compression algorithm we support;
///
/// 3. the Media type in the Content-Type response header is text with any
///    subtype (e.g. text/plain) or application/json.
///
/// `compress_response` returns Ok on successful compression, or if not all three
/// conditions above are met. It returns Err if there was a problem decoding
/// a header, or if compression itself fails; in that case res keeps its
/// original, uncompressed body.
///
/// This function logs errors to stderr, but only in debug mode. No information
/// is logged in release builds.
async fn compress_response(accept_encoding: Option<&header::HeaderValue>, res: &mut Response<Body>) -> Result<(), String> {
	// Check if the data is eligible for compression.
	if let Some(hdr) = res.headers().get(header::CONTENT_TYPE) {
		match from_utf8(hdr.as_bytes()) {
			Ok(s) => {
				// TODO: better determination of what is eligible for compression
				if !(s.starts_with("text/") || s.starts_with("application/json")) {
					return Ok(());
				};
			}
			Err(e) => {
				dbg_msg!(e);
				return Err(e.to_string());
			}
		};
	} else {
		// Response declares no Content-Type. Assume for simplicity that it
		// cannot be compressed.
		return Ok(());
	};

	// Don't bother if the size of the size of the response body will fit
	// within an IP frame (less the bytes that make up the TCP/IP and HTTP
	// headers).
	if res.body().size_hint().lower() < 1452 {
		return Ok(());
	};

	// Check to see which compressor is requested, and if we can use it.
	let accept_encoding: &str = match accept_encoding {
		None => return Ok(()), // Client requested no compression.

		Some(hdr) => match from_utf8(hdr.as_bytes()) {
			Ok(val) => val,

			#[cfg(debug_assertions)]
			Err(e) => {
				dbg_msg!(e);
				return Ok(());
			}

			#[cfg(not(debug_assertions))]
			Err(_) => return Ok(()),
		},
	};

	let compressor: CompressionType = match determine_compressor(accept_encoding) {
		Some(c) => c,
		None => return Ok(()),
	};

	// Get the body from the response.
	let body_bytes = match body::to_bytes(res.body_mut()).await {
		Ok(b) => b,
		Err(e) => {
			dbg_msg!(e);
			return Err(e.to_string());
		}
	};

	// Compress! This is CPU-bound (brotli at its default quality especially),
	// so run it on the blocking pool instead of stalling a runtime worker.
	let input = body_bytes.to_vec();
	let compressed = tokio::task::spawn_blocking(move || compress_body(compressor, input))
		.await
		.map_err(|e| e.to_string())
		.and_then(|res| res);

	match compressed {
		Ok(compressed) => {
			// We get here iff the compression was successful. Replace the body
			// with the compressed payload, and add the appropriate
			// Content-Encoding header in the response. Remove any precomputed
			// Content-Length, as it will no longer be valid.
			let headers = res.headers_mut();
			headers.insert(header::CONTENT_ENCODING, header::HeaderValue::from_static(compressor.as_str()));
			headers.remove(header::CONTENT_LENGTH);

			*(res.body_mut()) = Body::from(compressed);
			Ok(())
		}

		Err(e) => {
			// `to_bytes` drained the body. Put it back so the client still
			// gets the uncompressed response instead of an empty one.
			dbg_msg!(e);
			*(res.body_mut()) = Body::from(body_bytes);
			Err(e)
		}
	}
}

/// Compresses a `Vec<u8>` given a [`CompressionType`].
///
/// This is a helper function for [`compress_response`] and should not be
/// called directly.

// I've chosen a TTL of 600 (== 10 minutes) since compression is
// computationally expensive and we don't want to be doing it often. This is
// larger than client::json's TTL, but that's okay, because if client::json
// returns a new serde_json::Value, body_bytes changes, so this function will
// execute again.
#[cached(size = 100, time = 600, result = true)]
fn compress_body(compressor: CompressionType, body_bytes: Vec<u8>) -> Result<Vec<u8>, String> {
	// io::Cursor implements io::Read, required for our encoders.
	let mut reader = io::Cursor::new(body_bytes);

	let compressed: Vec<u8> = match compressor {
		CompressionType::Gzip => {
			let mut gz: gzip::Encoder<Vec<u8>> = match gzip::Encoder::new(Vec::new()) {
				Ok(gz) => gz,
				Err(e) => {
					dbg_msg!(e);
					return Err(e.to_string());
				}
			};

			match io::copy(&mut reader, &mut gz) {
				Ok(_) => match gz.finish().into_result() {
					Ok(compressed) => compressed,
					Err(e) => {
						dbg_msg!(e);
						return Err(e.to_string());
					}
				},
				Err(e) => {
					dbg_msg!(e);
					return Err(e.to_string());
				}
			}
		}

		CompressionType::Brotli => {
			// Quality 5 instead of the default 11: on a 220 KB subreddit page,
			// 11 took ~165 ms of CPU against ~4 ms for 5, for output only ~15%
			// smaller (33.5 KB vs 38.7 KB). Still beats gzip -6 (41.8 KB).
			let brotli_params = BrotliEncoderParams {
				quality: BROTLI_QUALITY,
				..BrotliEncoderParams::default()
			};

			let mut compressed = Vec::<u8>::new();
			match BrotliCompress(&mut reader, &mut compressed, &brotli_params) {
				Ok(_) => compressed,
				Err(e) => {
					dbg_msg!(e);
					return Err(e.to_string());
				}
			}
		}

		// This arm is for any requested compressor for which we don't yet
		// have an implementation.
		CompressionType::Passthrough => {
			let msg = "unsupported compressor".to_string();
			return Err(msg);
		}
	};

	Ok(compressed)
}

#[cfg(test)]
mod tests {
	use super::*;
	use brotli::Decompressor as BrotliDecompressor;
	use lipsum::lipsum;
	use std::{boxed::Box, io};

	#[test]
	fn test_client_ip() {
		let cf = header::HeaderName::from_static("cf-connecting-ip");
		let headers = |pairs: &[(&'static str, &'static str)]| {
			let mut map = HeaderMap::new();
			for (name, value) in pairs {
				map.insert(*name, header::HeaderValue::from_static(value));
			}
			map
		};

		// Behind Cloudflare: a client-forged X-Forwarded-For is ignored.
		let forged = headers(&[("x-forwarded-for", "6.6.6.6, 203.0.113.7"), ("cf-connecting-ip", "203.0.113.7")]);
		assert_eq!(client_ip(&forged, Some(&cf)), Some("203.0.113.7"));
		// With the trusted header configured but missing (e.g. a kubelet probe), there is no IP.
		assert_eq!(client_ip(&headers(&[("x-forwarded-for", "6.6.6.6")]), Some(&cf)), None);

		// Default: first X-Forwarded-For entry, then X-Real-IP.
		assert_eq!(client_ip(&forged, None), Some("6.6.6.6"));
		assert_eq!(client_ip(&headers(&[("x-real-ip", " 198.51.100.2 ")]), None), Some("198.51.100.2"));
		assert_eq!(client_ip(&headers(&[("x-forwarded-for", " , 1.2.3.4")]), None), None);
		assert_eq!(client_ip(&HeaderMap::new(), None), None);
	}

	#[test]
	fn test_determine_compressor() {
		// Single compressor given.
		assert_eq!(determine_compressor("unsupported"), None);
		assert_eq!(determine_compressor("gzip"), Some(CompressionType::Gzip));
		assert_eq!(determine_compressor("*"), Some(DEFAULT_COMPRESSOR));

		// Multiple compressors.
		assert_eq!(determine_compressor("gzip, br"), Some(CompressionType::Brotli));
		assert_eq!(determine_compressor("gzip;q=0.8, br;q=0.3"), Some(CompressionType::Gzip));
		assert_eq!(determine_compressor("br, gzip"), Some(CompressionType::Brotli));
		assert_eq!(determine_compressor("br;q=0.3, gzip;q=0.4"), Some(CompressionType::Gzip));

		// Invalid q-values.
		assert_eq!(determine_compressor("gzip;q=NAN"), None);
	}

	#[tokio::test]
	async fn test_compress_response() {
		// This macro generates an Accept-Encoding header value given any number of
		// compressors.
		macro_rules! ae_gen {
			($x:expr) => {
				$x.to_string().as_str()
			};

			($x:expr, $($y:expr),+) => {
				format!("{}, {}", $x.to_string(), ae_gen!($($y),+)).as_str()
			};
		}

		for accept_encoding in [
			"*",
			ae_gen!(CompressionType::Gzip),
			ae_gen!(CompressionType::Brotli, CompressionType::Gzip),
			ae_gen!(CompressionType::Brotli),
		] {
			// Determine what the expected encoding should be based on both the
			// specific encodings we accept.
			let expected_encoding: CompressionType = match determine_compressor(accept_encoding) {
				Some(s) => s,
				None => panic!("determine_compressor(accept_encoding) => None"),
			};

			// Build our Accept-Encoding header.
			let accept_encoding_hdr = header::HeaderValue::from_str(accept_encoding).unwrap();

			// Build test response.
			let lorem_ipsum: String = lipsum(10000);
			let expected_lorem_ipsum = Vec::<u8>::from(lorem_ipsum.as_str());
			let mut res = Response::builder()
				.status(200)
				.header(header::CONTENT_TYPE, "text/plain")
				.body(Body::from(lorem_ipsum))
				.unwrap();

			// Perform the compression.
			if let Err(e) = compress_response(Some(&accept_encoding_hdr), &mut res).await {
				panic!("compress_response(Some(&accept_encoding_hdr), &mut res) => Err(\"{e}\")");
			};

			// If the content was compressed, we expect the Content-Encoding
			// header to be modified.
			assert_eq!(
				res
					.headers()
					.get(header::CONTENT_ENCODING)
					.unwrap_or_else(|| panic!("missing content-encoding header"))
					.to_str()
					.unwrap_or_else(|_| panic!("failed to convert Content-Encoding header::HeaderValue to String")),
				expected_encoding.to_string()
			);

			// Decompress body and make sure it's equal to what we started
			// with.
			//
			// In the case of no compression, just make sure the "new" body in
			// the Response is the same as what with which we start.
			let body_vec = match body::to_bytes(res.body_mut()).await {
				Ok(b) => b.to_vec(),
				Err(e) => panic!("{e}"),
			};

			if expected_encoding == CompressionType::Passthrough {
				assert!(body_vec.eq(&expected_lorem_ipsum));
				continue;
			}

			// This provides an io::Read for the underlying body.
			let mut body_cursor: io::Cursor<Vec<u8>> = io::Cursor::new(body_vec);

			// Match the appropriate decompresor for the given
			// expected_encoding.
			let mut decoder: Box<dyn io::Read> = match expected_encoding {
				CompressionType::Gzip => match gzip::Decoder::new(&mut body_cursor) {
					Ok(dgz) => Box::new(dgz),
					Err(e) => panic!("{e}"),
				},

				CompressionType::Brotli => Box::new(BrotliDecompressor::new(body_cursor, expected_lorem_ipsum.len())),

				_ => panic!("no decompressor for {expected_encoding}"),
			};

			let mut decompressed = Vec::<u8>::new();
			if let Err(e) = io::copy(&mut decoder, &mut decompressed) {
				panic!("{e}");
			};

			assert!(decompressed.eq(&expected_lorem_ipsum));
		}
	}
}
