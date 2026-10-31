#![allow(clippy::cmp_owned)]
use crate::utils::{self, catch_random, error, filter_posts, format_num, format_url, get_filters, param, redirect, setting, template, val, Post, Preferences};
use crate::{
	client::json,
	server::RequestExt,
	subreddit::{can_access_quarantine, quarantine},
};
use askama::Template;
use hyper::{Body, Request, Response};
use regex::Regex;
use std::sync::LazyLock;

// STRUCTS
struct SearchParams {
	/// The query as typed. Askama escapes it where it is shown.
	q: String,
	/// The query percent-encoded, for links and Reddit requests.
	q_url: String,
	sort: String,
	t: String,
	before: String,
	after: String,
	restrict_sr: String,
	typed: String,
}

// STRUCTS
struct Subreddit {
	name: String,
	url: String,
	icon: String,
	description: String,
	subscribers: (String, String),
}

#[derive(Template)]
#[template(path = "search.html")]
struct SearchTemplate {
	posts: Vec<Post>,
	subreddits: Vec<Subreddit>,
	sub: String,
	params: SearchParams,
	prefs: Preferences,
	url: String,
	/// Whether the subreddit itself is filtered.
	is_filtered: bool,
	/// Whether all fetched posts are filtered (to differentiate between no posts fetched in the first place,
	/// and all fetched posts being filtered).
	all_posts_filtered: bool,
	/// Whether all posts were hidden because they are NSFW (and user has disabled show NSFW)
	all_posts_hidden_nsfw: bool,
	no_posts: bool,
}

/// Regex matched against search queries to determine if they are reddit urls.
static REDDIT_URL_MATCH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^https?://([^\./]+\.)*reddit.com/").unwrap());

// SERVICES
pub async fn find(req: Request<Body>) -> Result<Response<Body>, String> {
	// This ensures that during a search, no NSFW posts are fetched at all
	let nsfw_results = if setting(&req, "show_nsfw") == "on" && !utils::sfw_only() {
		"&include_over_18=on"
	} else {
		""
	};
	let uri_path = req.uri().path().replace("+", "%2B");
	let post_count = setting(&req, "post_count");
	let limit: u32 = post_count.parse().unwrap_or(50).clamp(1, 100);
	let path = format!("{}.json?{}{}&raw_json=1&limit={limit}", uri_path, req.uri().query().unwrap_or_default(), nsfw_results);
	let mut query = param(&path, "q").unwrap_or_default();
	query = REDDIT_URL_MATCH.replace(&query, "").to_string();

	if query.is_empty() {
		return Ok(redirect("/"));
	}

	if query.starts_with("r/") || query.starts_with("user/") {
		return Ok(redirect(&format!("/{query}")));
	}

	if query.starts_with("R/") {
		return Ok(redirect(&format!("/r{}", &query[1..])));
	}

	if query.starts_with("u/") || query.starts_with("U/") {
		return Ok(redirect(&format!("/user{}", &query[1..])));
	}

	let sub = req.param("sub").unwrap_or_default();
	let quarantined = can_access_quarantine(&req, &sub);
	// Handle random subreddits
	if let Ok(random) = catch_random(&sub, "/find").await {
		return Ok(random);
	}

	let typed = param(&path, "type").unwrap_or_default();
	let q_url = encode_query(&query);

	let sort = param(&path, "sort").unwrap_or_else(|| "relevance".to_string());
	let filters = get_filters(&req);

	// If search is not restricted to this subreddit, show other subreddits in search results
	let subreddits = if param(&path, "restrict_sr").is_none() {
		let mut subreddits = search_subreddits(&q_url, &typed).await;
		subreddits.retain(|s| !filters.contains(s.name.as_str()));
		subreddits
	} else {
		Vec::new()
	};

	let url = String::from(req.uri().path_and_query().map_or("", |val| val.as_str()));

	// If all requested subs are filtered, we don't need to fetch posts.
	if sub.split('+').all(|s| filters.contains(s)) {
		Ok(template(&SearchTemplate {
			posts: Vec::new(),
			subreddits,
			sub,
			params: SearchParams {
				q: query,
				q_url,
				sort,
				t: param(&path, "t").unwrap_or_default(),
				before: param(&path, "after").unwrap_or_default(),
				after: String::new(),
				restrict_sr: param(&path, "restrict_sr").unwrap_or_default(),
				typed,
			},
			prefs: Preferences::new(&req),
			url,
			is_filtered: true,
			all_posts_filtered: false,
			all_posts_hidden_nsfw: false,
			no_posts: false,
		}))
	} else {
		match Post::fetch(&path, quarantined).await {
			Ok((mut posts, after)) => {
				let (_, all_posts_filtered) = filter_posts(&mut posts, &filters);
				let no_posts = posts.is_empty();
				let all_posts_hidden_nsfw = !no_posts && (posts.iter().all(|p| p.flags.nsfw) && setting(&req, "show_nsfw") != "on");
				Ok(template(&SearchTemplate {
					posts,
					subreddits,
					sub,
					params: SearchParams {
						q: query,
						q_url,
						sort,
						t: param(&path, "t").unwrap_or_default(),
						before: param(&path, "after").unwrap_or_default(),
						after,
						restrict_sr: param(&path, "restrict_sr").unwrap_or_default(),
						typed,
					},
					prefs: Preferences::new(&req),
					url,
					is_filtered: false,
					all_posts_filtered,
					all_posts_hidden_nsfw,
					no_posts,
				}))
			}
			Err(msg) => {
				if msg == "quarantined" || msg == "gated" {
					let sub = req.param("sub").unwrap_or_default();
					Ok(quarantine(&req, sub, &msg))
				} else {
					error(req, &msg).await
				}
			}
		}
	}
}

/// Percent-encodes a search query for use as a `q=` value in a link or request.
fn encode_query(q: &str) -> String {
	url::form_urlencoded::byte_serialize(q.as_bytes()).collect()
}

/// `q_url` is the query as made by [`encode_query`].
async fn search_subreddits(q_url: &str, typed: &str) -> Vec<Subreddit> {
	let limit = if typed == "sr_user" { "50" } else { "3" };
	let subreddit_search_path = format!("/subreddits/search.json?q={q_url}&limit={limit}");

	// Send a request to the url
	json(subreddit_search_path, false).await.unwrap_or_default()["data"]["children"]
		.as_array()
		.map(ToOwned::to_owned)
		.unwrap_or_default()
		.iter()
		.map(|subreddit| {
			// For each subreddit from subreddit list
			// Fetch subreddit icon either from the community_icon or icon_img value
			let icon = subreddit["data"]["community_icon"].as_str().map_or_else(|| val(subreddit, "icon_img"), ToString::to_string);

			Subreddit {
				name: val(subreddit, "display_name"),
				url: val(subreddit, "url"),
				icon: format_url(&icon),
				description: val(subreddit, "public_description"),
				subscribers: format_num(subreddit["data"]["subscribers"].as_f64().unwrap_or_default() as i64),
			}
		})
		.collect::<Vec<Subreddit>>()
}

#[cfg(test)]
mod tests {
	use super::encode_query;
	use crate::utils::{param, Flair};

	#[test]
	fn test_query_round_trips_through_a_link() {
		for q in ["c++", "AT&T", "C#", "100%", "say \"hi\"", "a b", "q=1&limit=100", "naïve"] {
			let link = format!("?q={}&sort=new", encode_query(q));
			assert_eq!(param(&link, "q").as_deref(), Some(q), "{link}");
			assert_eq!(param(&link, "sort").as_deref(), Some("new"), "{link}");
		}
	}

	#[test]
	fn test_flair_search_round_trips() {
		for text in ["Q&A", "C++", "50% off", "A#B", "Say \"hi\""] {
			let flair = Flair {
				flair_parts: Vec::new(),
				text: text.to_string(),
				background_color: String::new(),
				foreground_color: String::new(),
			};
			let link = format!("?q={}&restrict_sr=on", flair.search_query());
			assert_eq!(param(&link, "q"), Some(format!("flair_name:\"{text}\"")), "{link}");
			assert_eq!(param(&link, "restrict_sr").as_deref(), Some("on"), "{link}");
		}
	}
}
