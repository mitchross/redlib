#![allow(clippy::cmp_owned)]
use crate::config::get_setting;
use crate::server::RequestExt;
use crate::subreddit::{can_access_quarantine, quarantine};
use crate::utils::{
	error, format_num, get_filters, nsfw_landing, param, parse_post, rewrite_emotes, setting, template, time, val, Author, Awards, Comment, Flair, FlairPart, Post, Preferences,
};
use askama::Template;
use hyper::{Body, Request, Response};
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::sync::LazyLock;

// STRUCTS
#[derive(Template)]
#[template(path = "post.html")]
struct PostTemplate {
	comments: Vec<Comment>,
	post: Post,
	sort: String,
	prefs: Preferences,
	single_thread: bool,
	url: String,
	url_without_query: String,
	comment_query: String,
	next_cursor: String,
}

static COMMENT_SEARCH_CAPTURE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\?q=(.*)&type=comment").unwrap());

pub async fn item(req: Request<Body>) -> Result<Response<Body>, String> {
	// Build Reddit API path
	let mut path: String = format!("{}.json?{}&raw_json=1", req.uri().path(), req.uri().query().unwrap_or_default());
	let sub = req.param("sub").unwrap_or_default();
	let quarantined = can_access_quarantine(&req, &sub);
	let url = req.uri().to_string();

	// Set sort to sort query parameter
	let sort = param(&path, "sort").unwrap_or_else(|| {
		// Grab default comment sort method from Cookies
		let default_sort = setting(&req, "comment_sort");

		// If there's no sort query but there's a default sort, set sort to default_sort
		if default_sort.is_empty() {
			String::new()
		} else {
			path = format!("{}.json?{}&sort={}&raw_json=1", req.uri().path(), req.uri().query().unwrap_or_default(), default_sort);
			default_sort
		}
	});

	// Log the post ID being fetched in debug mode
	#[cfg(debug_assertions)]
	req.param("id").unwrap_or_default();

	let single_thread = req.param("comment_id").is_some();
	let highlighted_comment = &req.param("comment_id").unwrap_or_default();

	// Send a request to the url, receive JSON in response
	match crate::client::json(path, quarantined).await {
		// Otherwise, grab the JSON output from the request
		Ok(response) => {
			// Parse the JSON into Post and Comment structs
			let post = parse_post(&response[0]["data"]["children"][0]).await;

			let req_url = req.uri().to_string();
			// Return landing page if this post if this Reddit deems this post
			// NSFW, but we have also disabled the display of NSFW content
			// or if the instance is SFW-only.
			if post.nsfw && crate::utils::should_be_nsfw_gated(&req, &req_url) {
				return Ok(nsfw_landing(req, req_url).await.unwrap_or_default());
			}

			let query_body = match COMMENT_SEARCH_CAPTURE.captures(&url) {
				Some(captures) => captures.get(1).unwrap().as_str().replace("%20", " ").replace('+', " "),
				None => String::new(),
			};

			let query_string = format!("q={query_body}&type=comment");
			let form = url::form_urlencoded::parse(query_string.as_bytes()).collect::<HashMap<_, _>>();
			let query = form.get("q").unwrap().clone().to_string();

			// Every comment shares these, so build them once instead of per comment
			let prefs = Preferences::new(&req);
			let comment_sort = param(&req.uri().to_string(), "sort").unwrap_or_else(|| setting(&req, "comment_sort"));

			let comments = match query.as_str() {
				"" => parse_comments(
					&response[1],
					&post.permalink,
					&post.author.name,
					highlighted_comment,
					&get_filters(&req),
					&prefs,
					&comment_sort,
					0,
				),
				_ => query_comments(
					&response[1],
					&post.permalink,
					&post.author.name,
					highlighted_comment,
					&get_filters(&req),
					&query.to_lowercase(),
					&prefs,
					&comment_sort,
					0,
				),
			};

			// Use the Post and Comment structs to generate a website to show users
			Ok(template(&PostTemplate {
				comments,
				post,
				url_without_query: url.clone().trim_end_matches(&format!("?q={query}&type=comment")).to_string(),
				sort: response[1]["data"]["sort"].as_str().unwrap_or(&sort).to_string(),
				prefs,
				single_thread,
				url: req_url,
				comment_query: query,
				next_cursor: response[1]["data"]["after"].as_str().unwrap_or_default().to_string(),
			}))
		}
		// If the Reddit API returns an error, exit and send error page to user
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

// COMMENTS

#[allow(clippy::too_many_arguments)]
fn parse_comments(
	json: &serde_json::Value,
	post_link: &str,
	post_author: &str,
	highlighted_comment: &str,
	filters: &HashSet<String>,
	prefs: &Preferences,
	comment_sort: &str,
	depth: u32,
) -> Vec<Comment> {
	// Parse the comment JSON into a Vector of Comments
	let comments = json["data"]["children"].as_array().map_or(Vec::new(), std::borrow::ToOwned::to_owned);

	// For each comment, retrieve the values to build a Comment object
	comments
		.into_iter()
		.map(|comment| {
			let data = &comment["data"];
			let replies: Vec<Comment> = if data["replies"].is_object() {
				parse_comments(&data["replies"], post_link, post_author, highlighted_comment, filters, prefs, comment_sort, depth + 1)
			} else {
				Vec::new()
			};
			build_comment(&comment, data, replies, post_link, post_author, highlighted_comment, filters, prefs, comment_sort, depth)
		})
		.collect()
}

#[allow(clippy::too_many_arguments)]
fn query_comments(
	json: &serde_json::Value,
	post_link: &str,
	post_author: &str,
	highlighted_comment: &str,
	filters: &HashSet<String>,
	query_lowercase: &str,
	prefs: &Preferences,
	comment_sort: &str,
	depth: u32,
) -> Vec<Comment> {
	let comments = json["data"]["children"].as_array().map_or(Vec::new(), std::borrow::ToOwned::to_owned);
	let mut results = Vec::new();

	for comment in comments {
		let data = &comment["data"];

		// If this comment contains replies, handle those too
		if data["replies"].is_object() {
			results.append(&mut query_comments(
				&data["replies"],
				post_link,
				post_author,
				highlighted_comment,
				filters,
				query_lowercase,
				prefs,
				comment_sort,
				depth + 1,
			));
		}

		// Search the comment's plain text; the rendered HTML would match `div`, `class` and `&amp;`
		if val(&comment, "body").to_lowercase().contains(query_lowercase) {
			results.push(build_comment(
				&comment,
				data,
				Vec::new(),
				post_link,
				post_author,
				highlighted_comment,
				filters,
				prefs,
				comment_sort,
				depth,
			));
		}
	}

	results
}
#[allow(clippy::too_many_arguments)]
fn build_comment(
	comment: &serde_json::Value,
	data: &serde_json::Value,
	replies: Vec<Comment>,
	post_link: &str,
	post_author: &str,
	highlighted_comment: &str,
	filters: &HashSet<String>,
	prefs: &Preferences,
	comment_sort: &str,
	depth: u32,
) -> Comment {
	let id = val(comment, "id");

	let body = if (val(comment, "author") == "[deleted]" && val(comment, "body") == "[removed]") || val(comment, "body") == "[ Removed by Reddit ]" {
		format!(
			"<div class=\"md\"><p>[removed] — <a href=\"https://{}{post_link}{id}\">view removed comment</a></p></div>",
			get_setting("REDLIB_PUSHSHIFT_FRONTEND").unwrap_or_else(|| String::from(crate::config::DEFAULT_PUSHSHIFT_FRONTEND)),
		)
	} else {
		rewrite_emotes(&data["media_metadata"], val(comment, "body_html"))
	};
	let kind = comment["kind"].as_str().unwrap_or_default().to_string();

	let unix_time = data["created_utc"].as_f64().unwrap_or_default();
	let (rel_time, created) = time(unix_time);

	let edited = data["edited"].as_f64().map_or((String::new(), String::new()), time);

	let score = data["score"].as_i64().unwrap_or(0);

	// The JSON API only provides comments up to some threshold.
	// Further comments have to be loaded by subsequent requests.
	// The "kind" value will be "more" and the "count"
	// shows how many more (sub-)comments exist in the respective nesting level.
	// Note that in certain (seemingly random) cases, the count is simply wrong.
	let more_count = data["count"].as_i64().unwrap_or_default();

	let awards: Awards = Awards::parse(&data["all_awardings"]);

	let parent_kind_and_id = val(comment, "parent_id");
	let parent_info = parent_kind_and_id.split('_').collect::<Vec<&str>>();

	let highlighted = id == highlighted_comment;

	let author = Author {
		name: val(comment, "author"),
		flair: Flair {
			flair_parts: FlairPart::parse(
				data["author_flair_type"].as_str().unwrap_or_default(),
				data["author_flair_richtext"].as_array(),
				data["author_flair_text"].as_str(),
			),
			text: val(comment, "link_flair_text"),
			background_color: val(comment, "author_flair_background_color"),
			foreground_color: val(comment, "author_flair_text_color"),
		},
		distinguished: val(comment, "distinguished"),
	};
	let is_filtered = filters.contains(&["u_", author.name.as_str()].concat());

	// Many subreddits have a default comment posted about the sub's rules etc.
	// Many Redlib users do not wish to see this kind of comment by default.
	// Reddit does not tell us which users are "bots", so a good heuristic is to
	// collapse stickied moderator comments.
	let is_moderator_comment = data["distinguished"].as_str().unwrap_or_default() == "moderator";
	let is_stickied = data["stickied"].as_bool().unwrap_or_default();
	let collapsed = (is_moderator_comment && is_stickied) || is_filtered;

	let descendant_count: u32 = replies.iter().map(|r| 1 + r.descendant_count).sum();
	let collapse_depth: u32 = if prefs.collapse_depth == "off" {
		u32::MAX
	} else if prefs.collapse_depth.is_empty() {
		2 // Default: collapse at depth 2
	} else {
		prefs.collapse_depth.parse().unwrap_or(2)
	};
	let auto_collapsed = depth >= collapse_depth;

	Comment {
		id,
		kind,
		comment_sort: comment_sort.to_string(),
		parent_id: parent_info[1].to_string(),
		parent_kind: parent_info[0].to_string(),
		post_link: post_link.to_string(),
		post_author: post_author.to_string(),
		body,
		author,
		score: if data["score_hidden"].as_bool().unwrap_or_default() {
			("\u{2022}".to_string(), "Hidden".to_string())
		} else {
			format_num(score)
		},
		rel_time,
		created,
		edited,
		replies,
		highlighted,
		awards,
		collapsed,
		is_filtered,
		more_count,
		hide_score: prefs.hide_score == "on",
		hide_awards: prefs.hide_awards == "on",
		depth,
		descendant_count,
		auto_collapsed,
	}
}

#[cfg(test)]
mod tests {
	use super::*;

	#[tokio::test]
	async fn single_thread_script_loads_for_every_post_type() {
		for post_type in ["image", "video", "gif", "link", "self"] {
			for single_thread in [true, false] {
				let mut post = parse_post(&serde_json::json!({"data": {"id": "abc", "subreddit": "x", "permalink": "/r/x/comments/abc/t/", "title": "t", "author": "someone"}})).await;
				post.post_type = post_type.to_string();
				let page = PostTemplate {
					comments: Vec::new(),
					post,
					sort: String::new(),
					prefs: Preferences::default(),
					single_thread,
					url: "/r/x/comments/abc/t/def".to_string(),
					url_without_query: String::new(),
					comment_query: String::new(),
					next_cursor: String::new(),
				};
				assert_eq!(page.render().unwrap().contains("/highlighted.js"), single_thread, "{post_type}");
			}
		}
	}

	fn comment(id: &str, body: &str, body_html: &str, replies: serde_json::Value) -> serde_json::Value {
		serde_json::json!({"kind": "t1", "data": {"id": id, "author": "someone", "body": body, "body_html": body_html, "parent_id": "t3_abc", "replies": replies}})
	}

	#[test]
	fn comment_search_matches_the_text_not_the_html() {
		let reply = comment("b", "Tom & Jerry", "<div class=\"md\"><p>Tom &amp; Jerry</p></div>", serde_json::json!(""));
		let thread = serde_json::json!({"data": {"children": [
			comment("a", "nothing here", "<div class=\"md\"><p>nothing here</p></div>", serde_json::json!({"data": {"children": [reply]}})),
		]}});
		let search = |query: &str| {
			let ids = |comments: Vec<crate::utils::Comment>| comments.into_iter().map(|c| c.id).collect::<Vec<_>>();
			ids(query_comments(
				&thread,
				"/r/x/comments/abc/t/",
				"op",
				"",
				&HashSet::new(),
				&query.to_lowercase(),
				&Preferences::default(),
				"new",
				0,
			))
		};

		// The rendered HTML contains these, the comments' text does not
		assert!(search("div").is_empty());
		assert!(search("class").is_empty());
		assert!(search("&amp;").is_empty());
		// A nested reply is found by its text, whatever the case
		assert_eq!(search("tom & JERRY"), vec!["b"]);
	}
}
