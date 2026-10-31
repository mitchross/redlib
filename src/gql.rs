// Comments for post pages from Reddit's GraphQL API (gql-fed.reddit.com), which
// the Android app uses. The `/comments/` JSON API is blocked for anonymous
// tokens since 2026-10-20.
//
// Adapted from polak14's implementation (AGPL-3.0-only):
// https://github.com/polak14/redlib/commit/29f18352cf54be4ffe579f773552acd0c45cdd08
//
// The post itself comes from `/by_id`, which gql_feed.rs answers. Comments are
// converted into the shape of the `/comments/` JSON response, so post.rs and
// the templates work unchanged.

use crate::client::{json, recover_from_block, roll_over_if_low, CLIENT, OAUTH_CLIENT, OAUTH_RATELIMIT_REMAINING};
use crate::utils::{format_url, param};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use chrono::DateTime;
use log::debug;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde_json::{json as json_value, Value};
use std::time::Duration;

const GQL_URL: &str = "https://gql-fed.reddit.com/";

// Persisted query ID from the Android app; Reddit rejects raw query text.
// Source: github.com/Amqx/reddit-gql `src/operations.ts`, or the APK's Apollo classes.
const POST_COMMENTS_HASH: &str = "314f29c2494fd78c7844ef2c6b09acb4bef5b37ab50b4ddc01691d6d5c8433f8";
// Reddit returns at most 200 comments per request, filled breadth-first
const COMMENT_COUNT: u32 = 200;
const MAX_DEPTH: u32 = 10;
const TIMEOUT: Duration = Duration::from_secs(20);
// Longer ones aren't Reddit's
pub(crate) const MAX_CURSOR_LEN: usize = 8192;

/// Builds a `/comments/` JSON response for `path` (a `/comments/` API path, as
/// built by `post::item`). Supports the `sort`, `cursor` and `context` query
/// parameters and single comment threads. Without a sort, comments use the
/// post's suggested sort, which the comments listing's `sort` reports.
pub async fn thread(path: &str, quarantine: bool) -> Result<Value, String> {
	let (id, comment_id) = parse_path(path).ok_or_else(|| format!("Couldn't parse post path | {path}"))?;
	let requested = param(path, "sort").filter(|s| !s.is_empty());
	if let Some(sort) = requested.as_deref().filter(|s| sort_type(s).is_none()) {
		return Err(format!("Unsupported comment sort: {sort}"));
	}

	let post = futures_lite::future::zip(json(format!("/by_id/t3_{id}.json?raw_json=1"), quarantine), crate::gql_feed::duplicate_count(&id));
	let (post, duplicates, sort, comments) = if let Some(sort) = requested {
		let ((post, duplicates), comments) = futures_lite::future::zip(post, comments(&id, comment_id.as_deref(), path, &sort)).await;
		(post, duplicates, sort, comments)
	} else {
		// The suggested sort comes with the post
		let (post, duplicates) = post.await;
		let post = post?;
		let suggested = post["data"]["children"][0]["data"]["suggested_sort"].as_str().filter(|s| sort_type(s).is_some());
		let sort = suggested.unwrap_or("confidence").to_string();
		let comments = comments(&id, comment_id.as_deref(), path, &sort).await;
		(Ok(post), duplicates, sort, comments)
	};
	let mut post = post?;
	// For the post's "duplicates" link
	post["data"]["children"][0]["data"]["num_duplicates"] = duplicates.into();
	let mut comments = listing(comments?);
	comments["data"]["sort"] = sort.into();
	Ok(json_value!([post, comments]))
}

/// GraphQL's comment sort for one of the JSON API's
fn sort_type(sort: &str) -> Option<&'static str> {
	Some(match sort {
		"confidence" | "best" => "CONFIDENCE",
		"top" => "TOP",
		"new" => "NEW",
		"controversial" => "CONTROVERSIAL",
		"old" => "OLD",
		"qa" => "QA",
		_ => return None,
	})
}

async fn comments(id: &str, comment_id: Option<&str>, path: &str, sort: &str) -> Result<Vec<Value>, String> {
	let variables = variables(id, comment_id, path, sort)?;
	let response = query("PostComments", POST_COMMENTS_HASH, &variables).await?;
	let trees = response["data"]["postInfoById"]["commentForest"]["trees"]
		.as_array()
		.ok_or_else(|| format!("No comments in response: {}", response["errors"]))?;
	let comments = nest(trees.iter().filter_map(|tree| item(tree, id)).collect());

	// With a context, Reddit also sends the focused comment's ancestors with all
	// their replies. Keep only the comment's branch, up to `context` ancestors.
	let context = context(path);
	match comment_id {
		Some(comment_id) if context > 0 && variables["after"].is_null() => {
			let (mut depth, mut branch) = focused_branch(&comments, comment_id).ok_or("Comment not found")?;
			while depth > context {
				branch = branch["data"]["replies"]["data"]["children"][0].take();
				depth -= 1;
			}
			Ok(vec![branch])
		}
		_ => Ok(comments),
	}
}

fn context(path: &str) -> usize {
	param(path, "context").and_then(|c| c.parse().ok()).unwrap_or(0)
}

fn variables(id: &str, comment_id: Option<&str>, path: &str, sort: &str) -> Result<Value, String> {
	let mut variables = json_value!({
		"id": format!("t3_{id}"),
		"sortType": sort_type(sort).ok_or_else(|| format!("Unsupported comment sort: {sort}"))?,
		"count": COMMENT_COUNT,
		"maxDepth": MAX_DEPTH,
		"includeConsistencyToken": false,
		"includePremiumAvatarTreatment": false,
	});
	// Links carry cursors base64-encoded
	let cursor = param(path, "cursor");
	if let Some(cursor) = &cursor {
		let cursor = Some(cursor)
			.filter(|c| c.len() <= MAX_CURSOR_LEN)
			.and_then(|c| STANDARD.decode(c).ok())
			.and_then(|c| String::from_utf8(c).ok())
			.ok_or("Invalid comments cursor")?;
		variables["after"] = cursor.into();
	}
	// A cursor already says where its comments go; with a focused comment as
	// well, Reddit fails with DEPENDENCY_FAILURE
	if let Some(comment_id) = comment_id.filter(|_| cursor.is_none()) {
		variables["focusedCommentContext"] = json_value!({ "commentId": format!("t1_{comment_id}"), "isFocusedThreadOnly": context(path) == 0 });
	}
	Ok(variables)
}

/// The comment `id` with its ancestors, without their other replies, and how
/// deep it is.
fn focused_branch(comments: &[Value], id: &str) -> Option<(usize, Value)> {
	for comment in comments {
		if comment["data"]["id"] == id {
			return Some((0, comment.clone()));
		}
		if let Some(replies) = comment["data"]["replies"]["data"]["children"].as_array() {
			if let Some((depth, branch)) = focused_branch(replies, id) {
				let mut parent = comment.clone();
				parent["data"]["replies"] = listing(vec![branch]);
				return Some((depth + 1, parent));
			}
		}
	}
	None
}

/// Runs a persisted query. Fields that time out upstream come back null with an
/// error next to the data; one retry usually fills them in.
pub(crate) async fn query(operation: &str, hash: &str, variables: &Value) -> Result<Value, String> {
	let body = json_value!({
		"operationName": operation,
		"variables": variables,
		"extensions": { "persistedQuery": { "version": 1, "sha256Hash": hash } },
	});

	let mut response = Value::Null;
	for _ in 0..2 {
		let mut builder = CLIENT.load().post(GQL_URL).timeout(TIMEOUT);
		for (key, value) in &OAUTH_CLIENT.load_full().headers_map {
			// `json()` sets its own; a duplicate Content-Type fails every query
			if !key.eq_ignore_ascii_case("content-type") {
				builder = builder.header(key, value);
			}
		}
		roll_over_if_low();
		let res = builder.json(&body).send().await.map_err(|e| format!("Couldn't send request to Reddit: {operation}: {e}"))?;
		let status = res.status();
		// GraphQL's own rate limit, 600 per token
		if let Some(remaining) = res.headers().get("x-ratelimit-remaining").and_then(|v| v.to_str().ok()?.parse::<f32>().ok()) {
			OAUTH_RATELIMIT_REMAINING.store(remaining.round() as u16, std::sync::atomic::Ordering::SeqCst);
		}
		let text = res.text().await.map_err(|e| format!("{operation} returned {status}, then failed: {e}"))?;
		// Errors come as JSON too, except Reddit's block page
		response = match serde_json::from_str(&text) {
			Ok(response) => response,
			Err(_) => {
				recover_from_block(status.as_u16());
				return Err(format!("Failed to parse page JSON data: {operation} returned {status}"));
			}
		};

		if response["data"].is_null() {
			return Err(format!("{operation} returned {status}: {}", response["errors"]));
		}
		if response["errors"].is_null() {
			break;
		}
		debug!("{operation} returned partial data, retrying: {}", response["errors"]);
	}
	Ok(response)
}

/// Converts one entry of the comment forest to an item of the JSON API, with its depth.
fn item(tree: &Value, post_id: &str) -> Option<(u64, Value)> {
	let depth = tree["depth"].as_u64()?;
	let post_fullname = format!("t3_{post_id}");
	let parent = tree["parentId"].as_str().filter(|p| p.starts_with("t1_")).unwrap_or(&post_fullname).to_string();

	let more = &tree["more"];
	if !more.is_null() {
		let count = more["count"].as_i64().unwrap_or_default();
		return Some((
			depth,
			if tree["parentId"].is_null() {
				// More top-level comments: rendered as a link to the next page of comments
				let cursor = STANDARD.encode(more["cursor"].as_str()?);
				json_value!({ "kind": "more_comments", "data": { "id": utf8_percent_encode(&cursor, NON_ALPHANUMERIC).to_string(), "count": count, "parent_id": post_fullname } })
			} else {
				json_value!({ "kind": "more", "data": { "id": "", "count": count, "parent_id": parent } })
			},
		));
	}

	let node = &tree["node"];
	let fullname = node["id"].as_str().unwrap_or_default();
	// IDs go into links and element IDs; deleted comments have none
	if node["__typename"] != "DeletedComment" && !fullname.strip_prefix("t1_").is_some_and(is_id) {
		return None;
	}
	let author = node["authorInfo"]["name"].as_str().unwrap_or("[unknown]");
	let content = &node["content"];

	// The JSON API's representation of deleted and removed comments, which post.rs looks for
	let (author, body, body_html) = if node["__typename"] == "DeletedComment" {
		("[deleted]", "[deleted]", "<p>[deleted]</p>".to_string())
	} else if node["isAdminTakedown"] == true {
		(author, "[ Removed by Reddit ]", String::new())
	} else if node["isRemoved"] == true {
		("[deleted]", "[removed]", String::new())
	} else {
		(author, content["markdown"].as_str().unwrap_or_default(), content_html(content))
	};

	let flair = &node["authorFlair"];
	let (flair_type, flair_richtext) = flair_parts(flair);

	Some((
		depth,
		json_value!({
			"kind": "t1",
			"data": {
				"id": fullname.strip_prefix("t1_").unwrap_or(fullname),
				"name": fullname,
				"author": author,
				"body": body,
				"body_html": format!("<div class=\"md\">{body_html}</div>"),
				"created_utc": timestamp(&node["createdAt"]).unwrap_or_default(),
				"edited": timestamp(&node["editedAt"]).map_or(Value::Bool(false), Value::from),
				"score": node["score"].as_i64().unwrap_or_default(),
				"score_hidden": node["isScoreHidden"] == true,
				"stickied": node["isStickied"] == true,
				"distinguished": node["distinguishedAs"].as_str().map(str::to_lowercase),
				"author_flair_type": flair_type,
				"author_flair_richtext": flair_richtext,
				"author_flair_text": flair["text"],
				"author_flair_background_color": flair["template"]["backgroundColor"],
				"author_flair_text_color": flair["textColor"].as_str().map(str::to_lowercase),
				"all_awardings": awards(&node["awardings"]),
				"parent_id": parent,
				"replies": "",
			}
		}),
	))
}

/// A flair's type and richtext, as the JSON API's `*_flair_type` and `*_flair_richtext`.
pub(crate) fn flair_parts(flair: &Value) -> (&'static str, Value) {
	let richtext: Value = flair["richtext"].as_str().and_then(|r| serde_json::from_str(r).ok()).unwrap_or_default();
	let kind = if richtext.as_array().is_some_and(|r| !r.is_empty()) {
		"richtext"
	} else if flair["text"].as_str().is_some_and(|t| !t.is_empty()) {
		"text"
	} else {
		""
	};
	(kind, richtext)
}

/// Awardings as the JSON API's `all_awardings`.
pub(crate) fn awards(awardings: &Value) -> Vec<Value> {
	awardings
		.as_array()
		.into_iter()
		.flatten()
		.map(|a| {
			// Feeds sometimes only have the animated icon
			let award = &a["award"];
			let icon = award["static_icon_16"]["url"].as_str().or(award["icon_16"]["url"].as_str());
			json_value!({ "name": award["name"], "count": a["total"], "resized_icons": [{ "url": icon }] })
		})
		.collect()
}

pub(crate) fn timestamp(value: &Value) -> Option<f64> {
	DateTime::parse_from_str(value.as_str()?, "%Y-%m-%dT%H:%M:%S%.f%z").ok().map(|t| t.timestamp() as f64)
}

/// Builds the reply tree from items in depth-first order. Goes by depth rather
/// than parent IDs, as deleted comments have no ID.
fn nest(items: Vec<(u64, Value)>) -> Vec<Value> {
	let mut top = Vec::new();
	let mut open: Vec<(u64, Value)> = Vec::new();
	for (depth, item) in items {
		while open.last().is_some_and(|(d, _)| *d >= depth) {
			close(&mut open, &mut top);
		}
		open.push((depth, item));
	}
	while !open.is_empty() {
		close(&mut open, &mut top);
	}
	top
}

/// Attaches the innermost open item to its parent, or to the top level.
fn close(open: &mut Vec<(u64, Value)>, top: &mut Vec<Value>) {
	let Some((_, item)) = open.pop() else { return };
	let Some((_, parent)) = open.last_mut() else {
		top.push(item);
		return;
	};
	let replies = &mut parent["data"]["replies"];
	if !replies.is_object() {
		*replies = listing(Vec::new());
	}
	if let Some(children) = replies["data"]["children"].as_array_mut() {
		children.push(item);
	}
}

/// Extracts the post ID and optional comment ID from paths like
/// `/r/{sub}/comments/{id}/{title}/{comment_id}.json?...`, also without the
/// subreddit or with `/user/{name}` in its place.
fn parse_path(path: &str) -> Option<(String, Option<String>)> {
	let path = path.split('?').next()?.strip_suffix(".json")?;
	let segments: Vec<&str> = path.trim_matches('/').split('/').collect();
	// By position, so a subreddit or title named "comments" isn't taken for the marker
	let rest = match segments.as_slice() {
		["comments", rest @ ..] => rest,
		["r" | "user" | "u", owner, "comments", rest @ ..] if !owner.is_empty() => rest,
		_ => return None,
	};
	let (id, comment_id) = match rest {
		[id] | [id, _] => (*id, None),
		[id, _, comment_id] => (*id, Some(*comment_id).filter(|c| !c.is_empty())),
		_ => return None,
	};
	if !is_id(id) || comment_id.is_some_and(|c| !is_id(c)) {
		return None;
	}
	Some((id.to_string(), comment_id.map(str::to_string)))
}

/// A post or comment ID (without its `t3_`/`t1_` prefix)
pub(crate) fn is_id(id: &str) -> bool {
	!id.is_empty() && id.bytes().all(|b| b.is_ascii_alphanumeric())
}

fn listing(children: Vec<Value>) -> Value {
	json_value!({ "kind": "Listing", "data": { "children": children } })
}

/// Escaped URL attribute value, proxied through Redlib for Reddit media. Empty
/// unless it's a web or mail link, a local path or a fragment.
fn url_attr(value: Option<&str>) -> String {
	let value = value.unwrap_or_default().trim();
	// Browsers drop control characters from URLs, so `java\nscript:` would run
	if value.chars().any(char::is_control) {
		return String::new();
	}
	// Not `//host` or `/\host`, which browsers treat as another site
	let local = (value.starts_with('/') && !value.starts_with("//") && !value.contains('\\')) || value.starts_with('#');
	let web = url::Url::parse(value).is_ok_and(|u| matches!(u.scheme(), "http" | "https" | "mailto"));
	if !local && !web {
		return String::new();
	}
	htmlescape::encode_minimal(&format_url(value))
}

/// A comment's, post's or page's `content` as HTML: its richtext, else its
/// markdown as escaped plain text.
pub(crate) fn content_html(content: &Value) -> String {
	let richtext: Value = content["richtext"].as_str().and_then(|r| serde_json::from_str(r).ok()).unwrap_or_default();
	if richtext["document"].is_array() {
		return render(&richtext["document"], &content["richtextMedia"]);
	}
	match content["markdown"].as_str().filter(|m| !m.is_empty()) {
		Some(markdown) => format!("<p>{}</p>", htmlescape::encode_minimal(markdown)),
		None => String::new(),
	}
}

/// Renders richtext (RTJSON) nodes as the HTML of the JSON API's `body_html`.
pub(crate) fn render(nodes: &Value, media: &Value) -> String {
	nodes.as_array().into_iter().flatten().map(|node| element(node, media)).collect()
}

fn element(node: &Value, media: &Value) -> String {
	let children = || render(&node["c"], media);
	match node["e"].as_str().unwrap_or_default() {
		// A paragraph of only images is a figure, like the JSON API's
		"par" if node["c"].as_array().is_some_and(|c| c.iter().all(|c| matches!(c["e"].as_str(), Some("img" | "gif")))) => children(),
		"par" => format!("<p>{}</p>", children()),
		"text" => formatted(node),
		"br" => "<br>".to_string(),
		"hr" => "<hr>".to_string(),
		"h" => {
			let level = node["l"].as_u64().unwrap_or(1).clamp(1, 6);
			format!("<h{level}>{}</h{level}>", children())
		}
		"blockquote" => format!("<blockquote>{}</blockquote>", children()),
		"list" => {
			let tag = if node["o"] == true { "ol" } else { "ul" };
			format!("<{tag}>{}</{tag}>", children())
		}
		"li" => format!("<li>{}</li>", children()),
		"code" => {
			let lines: Vec<&str> = node["c"].as_array().into_iter().flatten().map(|line| line["t"].as_str().unwrap_or_default()).collect();
			format!("<pre><code>{}</code></pre>", htmlescape::encode_minimal(&lines.join("\n")))
		}
		"spoilertext" => format!("<span class=\"md-spoiler-text\">{}</span>", children()),
		"link" => {
			let title = node["a"].as_str().map(|a| format!(" title=\"{}\"", htmlescape::encode_minimal(a))).unwrap_or_default();
			format!("<a href=\"{}\"{title}>{}</a>", url_attr(node["u"].as_str()), formatted(node))
		}
		kind @ ("r/" | "u/") => {
			let name = htmlescape::encode_minimal(node["t"].as_str().unwrap_or_default());
			let slash = if node["l"] == true { "/" } else { "" };
			format!("<a href=\"/{kind}{name}\">{slash}{kind}{name}</a>")
		}
		"img" | "gif" => {
			let Some(item) = media.as_array().and_then(|m| m.iter().find(|m| m["id"] == node["id"])) else {
				return String::new();
			};
			let src = url_attr(item["url"].as_str());
			format!("<figure><a href=\"{src}\"><img src=\"{src}\" loading=\"lazy\"></a></figure>")
		}
		"table" => table(node, media),
		// Unknown elements: keep their content
		_ if node["c"].is_array() => children(),
		_ => htmlescape::encode_minimal(node["t"].as_str().unwrap_or_default()),
	}
}

/// RTJSON format bits and their tags
const FORMATS: [(u64, &str); 6] = [(1, "strong"), (2, "em"), (8, "del"), (16, "sub"), (32, "sup"), (64, "code")];

/// Text with format ranges (`[bits, start, length]` in characters) as HTML.
fn formatted(node: &Value) -> String {
	let text: Vec<char> = node["t"].as_str().unwrap_or_default().chars().collect();
	let mut bits = vec![0u64; text.len()];
	for range in node["f"].as_array().into_iter().flatten() {
		let (Some(format), Some(start), Some(len)) = (range[0].as_u64(), range[1].as_u64(), range[2].as_u64()) else {
			continue;
		};
		for b in bits.iter_mut().skip(start as usize).take(len as usize) {
			*b |= format;
		}
	}

	let mut out = String::new();
	let mut start = 0;
	while start < text.len() {
		let end = (start..text.len()).find(|&i| bits[i] != bits[start]).unwrap_or(text.len());
		let tags: Vec<&str> = FORMATS.iter().filter(|(bit, _)| bits[start] & bit != 0).map(|(_, tag)| *tag).collect();
		tags.iter().for_each(|tag| out += &format!("<{tag}>"));
		out += &htmlescape::encode_minimal(&text[start..end].iter().collect::<String>());
		tags.iter().rev().for_each(|tag| out += &format!("</{tag}>"));
		start = end;
	}
	out
}

fn table(node: &Value, media: &Value) -> String {
	let align = |header: Option<&Value>| match header.and_then(|h| h["a"].as_str()) {
		Some("L") => " align=\"left\"",
		Some("C") => " align=\"center\"",
		Some("R") => " align=\"right\"",
		_ => "",
	};
	let headers = node["h"].as_array().map(Vec::as_slice).unwrap_or_default();

	let mut out = "<table><thead><tr>".to_string();
	for header in headers {
		out += &format!("<th{}>{}</th>", align(Some(header)), render(&header["c"], media));
	}
	out += "</tr></thead><tbody>";
	for row in node["c"].as_array().into_iter().flatten() {
		out += "<tr>";
		for (i, cell) in row.as_array().into_iter().flatten().enumerate() {
			out += &format!("<td{}>{}</td>", align(headers.get(i)), render(&cell["c"], media));
		}
		out += "</tr>";
	}
	out + "</tbody></table>"
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_parse_path() {
		assert_eq!(parse_path("/r/linux/comments/1xaxcyl/some_title/.json?&raw_json=1"), Some(("1xaxcyl".into(), None)));
		assert_eq!(parse_path("/r/linux/comments/1xaxcyl.json?sort=new&raw_json=1"), Some(("1xaxcyl".into(), None)));
		assert_eq!(
			parse_path("/r/linux/comments/1xaxcyl/some_title/19re6tdr7osth.json?&raw_json=1"),
			Some(("1xaxcyl".into(), Some("19re6tdr7osth".into())))
		);
		assert_eq!(parse_path("/comments/1xaxcyl/comments/abc.json?"), Some(("1xaxcyl".into(), Some("abc".into()))));
		assert_eq!(parse_path("/user/spez/comments/1xaxcyl.json?"), Some(("1xaxcyl".into(), None)));
		assert_eq!(parse_path("/r/comments/comments/1xdp2ww/.json"), Some(("1xdp2ww".into(), None)));
		assert_eq!(parse_path("/r/linux/comments/1xaxcyl/t/abc/.json?context=3"), Some(("1xaxcyl".into(), Some("abc".into()))));
		for path in [
			"/r/linux/hot.json",
			"/r/linux/comments/1xaxcyl",
			"/user/a/comments.json",
			"/r//comments/abc.json",
			"/comments/.json",
			"/comments/a/title/b/extra.json",
			"/comments/a%2fb.json",
		] {
			assert_eq!(parse_path(path), None, "{path}");
		}
	}

	#[test]
	fn test_url_attr() {
		for url in [
			"javascript:alert(1)",
			"JaVaScRiPt:alert(1)",
			"java\nscript:alert(1)",
			"data:text/html,<script>x</script>",
			"//evil.test/x",
			"/\\evil.test",
			"vbscript:msgbox(1)",
			"wiki/faq",
		] {
			assert!(url_attr(Some(url)).is_empty(), "{url}");
		}
		assert_eq!(url_attr(Some("/r/rust?a=1&b=2")), "/r/rust?a=1&amp;b=2");
		assert_eq!(url_attr(Some("#faq")), "#faq");
		assert_eq!(url_attr(Some("mailto:a@b.c")), "mailto:a@b.c");
		assert_eq!(url_attr(Some("https://i.redd.it/x.png")), "/img/x.png");
	}

	#[test]
	fn test_variables() {
		let cursor = STANDARD.encode("opaque:+/= cursor");
		for (sort, expected) in [
			("confidence", "CONFIDENCE"),
			("best", "CONFIDENCE"),
			("top", "TOP"),
			("new", "NEW"),
			("controversial", "CONTROVERSIAL"),
			("old", "OLD"),
			("qa", "QA"),
		] {
			let vars = variables("abc", Some("def"), &format!("?context=3&cursor={cursor}"), sort).unwrap();
			assert_eq!(vars["sortType"], expected);
			assert_eq!(vars["after"], "opaque:+/= cursor");
			// Reddit fails with DEPENDENCY_FAILURE for both
			assert!(vars["focusedCommentContext"].is_null());
		}
		assert_eq!(
			variables("abc", Some("def"), "?context=3", "top").unwrap()["focusedCommentContext"],
			json_value!({ "commentId": "t1_def", "isFocusedThreadOnly": false })
		);
		assert_eq!(variables("abc", Some("def"), "?", "top").unwrap()["focusedCommentContext"]["isFocusedThreadOnly"], true);
		assert!(variables("abc", None, "?cursor=%FF", "top").is_err());
		assert!(variables("abc", None, &format!("?cursor={}", "A".repeat(MAX_CURSOR_LEN + 4)), "top").is_err());
		assert!(variables("abc", None, "?", "invalid").is_err());
	}

	#[test]
	fn test_focused_branch() {
		let comment = |id: &str, depth: u64, parent: Option<&str>| json_value!({ "depth": depth, "parentId": parent, "node": { "__typename": "Comment", "id": id } });
		let trees = [
			comment("t1_a", 0, None),
			comment("t1_b", 1, Some("t1_a")),
			comment("t1_c", 2, Some("t1_b")),
			comment("t1_sibling", 1, Some("t1_a")),
			comment("t1_other", 0, None),
		];
		let comments = nest(trees.iter().filter_map(|t| item(t, "p")).collect());
		let (depth, branch) = focused_branch(&comments, "b").unwrap();
		assert_eq!(depth, 1);
		assert_eq!(branch["data"]["id"], "a");
		let replies = branch["data"]["replies"]["data"]["children"].as_array().unwrap();
		assert_eq!(replies.len(), 1);
		assert_eq!(replies[0]["data"]["id"], "b");
		assert_eq!(replies[0]["data"]["replies"]["data"]["children"][0]["data"]["id"], "c");
		assert!(focused_branch(&comments, "missing").is_none());
	}

	#[test]
	fn test_awards() {
		let awards = awards(&json_value!([
			{ "award": { "name": "A", "static_icon_16": { "url": "https://i.redd.it/a_16.png" }, "icon_16": { "url": "https://i.redd.it/a.gif" } }, "total": 1 },
			{ "award": { "name": "B", "static_icon_16": null, "icon_16": { "url": "https://i.redd.it/b_16.png" } }, "total": 2 },
		]));
		assert_eq!(awards[0]["resized_icons"][0]["url"], "https://i.redd.it/a_16.png");
		assert_eq!(awards[1]["resized_icons"][0]["url"], "https://i.redd.it/b_16.png");
	}

	#[test]
	fn test_item_checks() {
		let tree = json_value!({ "depth": 0, "parentId": "t3_p", "node": { "__typename": "Comment", "id": "t1_a", "content": { "markdown": "<script>x</script> & text" } } });
		let (_, comment) = item(&tree, "p").unwrap();
		// No richtext: escaped markdown
		assert_eq!(comment["data"]["body_html"], "<div class=\"md\"><p>&lt;script&gt;x&lt;/script&gt; &amp; text</p></div>");
		assert_eq!(comment["data"]["parent_id"], "t3_p");
		let mut bad = tree.clone();
		bad["node"]["id"] = "t1_a\"><x".into();
		assert!(item(&bad, "p").is_none());
	}

	#[test]
	fn test_render() {
		let doc = json_value!([
			{ "e": "par", "c": [
				{ "e": "text", "t": "a <b> ünï bold", "f": [[1, 10, 4], [3, 12, 2]] },
				{ "e": "link", "t": "x", "u": "javascript:alert(1)", "a": "t\"" },
				{ "e": "r/", "t": "rust", "l": true },
				{ "e": "spoilertext", "c": [{ "e": "text", "t": "s" }] },
			]},
			{ "e": "par", "c": [{ "e": "gif", "id": "giphy|g" }, { "e": "img", "id": "missing" }] },
			{ "e": "code", "c": [{ "e": "raw", "t": "<x>" }, { "e": "raw", "t": "y" }] },
			{ "e": "list", "o": true, "c": [{ "e": "li", "c": [{ "e": "par", "c": [{ "e": "text", "t": "i" }] }] }] },
			{ "e": "table", "h": [{ "a": "R", "c": [{ "e": "text", "t": "H" }] }, { "c": [] }], "c": [[{ "c": [{ "e": "text", "t": "1" }] }, { "c": [] }]] },
			{ "e": "future", "c": [{ "e": "text", "t": "kept" }] },
		]);
		let media = json_value!([{ "id": "giphy|g", "url": "https://external-preview.redd.it/g.gif?s=1&a=2" }]);
		assert_eq!(
			render(&doc, &media),
			concat!(
				r#"<p>a &lt;b&gt; ünï <strong>bo</strong><strong><em>ld</em></strong><a href="" title="t&quot;">x</a><a href="/r/rust">/r/rust</a><span class="md-spoiler-text">s</span></p>"#,
				r#"<figure><a href="/preview/external-pre/g.gif?s=1&amp;a=2"><img src="/preview/external-pre/g.gif?s=1&amp;a=2" loading="lazy"></a></figure>"#,
				"<pre><code>&lt;x&gt;\ny</code></pre>",
				"<ol><li><p>i</p></li></ol>",
				r#"<table><thead><tr><th align="right">H</th><th></th></tr></thead><tbody><tr><td align="right">1</td><td></td></tr></tbody></table>"#,
				"kept",
			)
		);
	}

	#[test]
	fn test_tree() {
		let comment = |id: &str, depth: u64, parent: Option<&str>| json_value!({ "depth": depth, "parentId": parent, "node": { "__typename": "Comment", "id": id, "authorInfo": { "name": "a" } } });
		let trees = [
			comment("t1_a", 0, None),
			// Deleted comments have no ID; their replies nest by depth
			json_value!({ "depth": 1, "parentId": "t1_a", "node": { "__typename": "DeletedComment" } }),
			comment("t1_c", 2, Some("t1_x")),
			json_value!({ "depth": 1, "parentId": "t1_a", "more": { "count": 5, "cursor": "c1" } }),
			comment("t1_d", 0, None),
			json_value!({ "depth": 0, "parentId": null, "more": { "count": 90, "cursor": "c2" } }),
		];
		let comments = nest(trees.iter().filter_map(|t| item(t, "p")).collect());
		assert_eq!(comments.len(), 3);

		let a = &comments[0]["data"];
		assert_eq!(a["parent_id"], "t3_p");
		let replies = a["replies"]["data"]["children"].as_array().unwrap();
		assert_eq!(replies.len(), 2);
		assert_eq!(replies[0]["data"]["author"], "[deleted]");
		assert_eq!(replies[0]["data"]["replies"]["data"]["children"][0]["data"]["id"], "c");
		assert_eq!(replies[1]["kind"], "more");
		assert_eq!(replies[1]["data"]["parent_id"], "t1_a");
		assert_eq!(replies[1]["data"]["count"], 5);

		assert_eq!(comments[1]["data"]["replies"], "");
		assert_eq!(comments[2]["kind"], "more_comments");
		// base64 of "c2", as links carry cursors
		assert_eq!(comments[2]["data"]["id"], "YzI%3D");
	}
}
