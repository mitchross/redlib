// Adapted from polak14's implementation (AGPL-3.0-only):
// https://github.com/polak14/redlib/commit/29f18352cf54be4ffe579f773552acd0c45cdd08
//
// Reddit's GraphQL API in place of the JSON API, which is blocked for anonymous
// tokens since 2026-10-28: subreddit feeds (also r/popular,
// r/all and combined feeds like r/a+b), posts by ID, subreddit and user info,
// user posts and comments, and search.
//
// `client::json` routes the matching JSON API paths here. Responses are
// converted into the JSON API's (listings of `t3` posts, `t1` comments, `t5`
// subreddits), so handlers and templates work unchanged. Paging cursors are
// fullnames (`t3_<id>`, `t1_<id>`), as in the JSON API; GraphQL takes them
// base64-encoded.

use crate::gql::is_id;
use crate::gql::{awards, content_html, flair_parts, query, timestamp, MAX_CURSOR_LEN};
use crate::utils::param;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use log::debug;
use regex::Regex;
use serde_json::{json as json_value, Value};
use std::collections::{HashMap, VecDeque};
use std::sync::LazyLock;

// Persisted query IDs from the Android app
const SUBREDDIT_FEED_HASH: &str = "97046e2a48050f10a507db354a8da60955ffccb586dcf63163bdfc6610d5cf7a";
const POPULAR_FEED_HASH: &str = "728c0f9a4d12c17b8cffe07669bb1a8770b0f2ec0c84e040485f8fad2d9c417f";
const ALL_FEED_HASH: &str = "0e9c0045dd244e934efd0ea4fefc00cb0b4d91523e2f09c8af075856cb59b937";
const POSTS_BY_IDS_HASH: &str = "0e471495bcba33554b6c540b3cf1cae23dd5432a8b741dddc7fd6717dc2520a7";
const SUBREDDIT_INFO_HASH: &str = "6b9c1679e69097e1c6df364adc11183afe6e2b6545dd1c0c1cc8f7490448c3e5";
const USER_HASH: &str = "4822704f40348a38e37626b62f7f2a4e4af88aa5ab90f271a0c539db7ab2ac2f";
const WIKI_HASH: &str = "32f9314683f033a76812f772bbba771958760286c5cd28c558071a3ff4eb231f";
// The only operation with a post's duplicates (`otherDiscussions`)
const DUPLICATES_HASH: &str = "bce3523d2f221bc90447c79b131eeff478eb3fa98d5cdd19a6ded37c2c046b87";
const USER_POSTS_HASH: &str = "78a213c02a340db4a707d4bcd375ce34f733b75893379603acb4ec20388fafe6";
const USER_COMMENTS_HASH: &str = "e1b37f521fb40af2120946b70e7fbf6a3d924db2abaf28ff2164d4cfacca28db";
const COMMENTS_BY_IDS_HASH: &str = "51ab9f2e0eb87b6dac9aac02012ba7046049e516ec55ec442abfed0f19c6f311";
const SEARCH_POSTS_HASH: &str = "8610e33521c90caa8d12576800c18b708ce0d19df3dfac8e2aad55d9860e4bf9";
const SEARCH_SUBREDDITS_HASH: &str = "a3333069c1ead3ceec6d9bf4da4185ff584f69d0ba8b586e545b32ea62170932";
const PAGE_SIZE: usize = 25;
// r/all returns ~7 posts on its first page and ~25 after, whatever is asked for
const ALL_MAX_REQUESTS: usize = 3;

static SUBREDDIT_PATH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^/r/([^/]+)/(\w+)\.json$").unwrap());
static USER_PATH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^/user/([^/]+)/(\w+)\.json$").unwrap());
static WIKI_PATH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^/r/([^/]+)/wiki/(.+)\.json$").unwrap());
static DUPLICATES_PATH: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^(?:/r/[^/]+)?/duplicates/(\w+)").unwrap());

const FEED_SORTS: [&str; 6] = ["hot", "new", "top", "rising", "controversial", "best"];
const USER_SORTS: [&str; 4] = ["new", "hot", "top", "controversial"];
const SEARCH_SORTS: [&str; 5] = ["relevance", "hot", "top", "new", "comments"];
const RANGES: [&str; 6] = ["hour", "day", "week", "month", "year", "all"];

/// Subreddit and user names: letters, digits, `_` (and `-` in user names)
fn is_name(name: &str, dash: bool) -> bool {
	!name.is_empty() && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_' || (dash && b == b'-'))
}

/// A sort or time range from a path, checked so Reddit's errors don't stand in for ours
fn choice(value: &str, allowed: &[&str], what: &str) -> Result<String, String> {
	let value = value.to_lowercase();
	if allowed.contains(&value.as_str()) {
		Ok(value)
	} else {
		Err(format!("Unsupported {what}: {value}"))
	}
}

fn range(path: &str) -> Result<Option<String>, String> {
	param(path, "t").filter(|t| !t.is_empty()).map(|t| choice(&t, &RANGES, "time range")).transpose()
}

fn after(path: &str) -> Result<String, String> {
	let after = param(path, "after").unwrap_or_default();
	if after.len() > MAX_CURSOR_LEN {
		return Err("Invalid cursor".to_string());
	}
	Ok(after)
}

/// Answers the JSON API paths that GraphQL covers, or `None` for other paths.
pub async fn route(path: &str, quarantine: bool) -> Option<Result<Value, String>> {
	let route = path.split('?').next().unwrap_or_default();
	if let Some(ids) = route.strip_prefix("/by_id/") {
		let ids: Vec<String> = ids.trim_end_matches(".json").split(',').map(str::to_string).collect();
		if !ids.iter().all(|id| id.strip_prefix("t3_").is_some_and(is_id)) {
			return Some(Err("Invalid post ID".to_string()));
		}
		return Some(by_id(ids).await.map(|posts| listing(posts, "")));
	}
	if route == "/search.json" {
		return Some(search(path, None).await);
	}
	if route == "/subreddits/search.json" {
		return Some(search_subreddits(path).await);
	}
	if let Some(caps) = WIKI_PATH.captures(route) {
		if !is_name(&caps[1], false) {
			return Some(Err("Invalid subreddit name".to_string()));
		}
		return Some(wiki(&caps[1], &caps[2]).await);
	}
	if let Some(caps) = DUPLICATES_PATH.captures(route) {
		if !is_id(&caps[1]) {
			return Some(Err("Invalid post ID".to_string()));
		}
		return Some(duplicates(&caps[1], &param(path, "sort").unwrap_or_default()).await);
	}

	if let Some(caps) = USER_PATH.captures(route) {
		let name = &caps[1];
		if !is_name(name, true) {
			return Some(Err("Invalid user name".to_string()));
		}
		let feed = || Feed::new(path, &param(path, "sort").filter(|s| !s.is_empty()).unwrap_or_else(|| "new".to_string()), &USER_SORTS);
		return Some(match &caps[2] {
			"about" => user(name).await,
			"submitted" => async { feed()?.user_posts(name, &after(path)?).await }.await.map(|(posts, after)| listing(posts, &after)),
			"comments" => async { feed()?.user_comments(name, &after(path)?).await }
				.await
				.map(|(comments, after)| listing(comments, &after)),
			"overview" => async { feed()?.overview(name).await }.await.map(|(items, after)| listing(items, &after)),
			_ => return None,
		});
	}

	let caps = SUBREDDIT_PATH.captures(route)?;
	let subs = caps[1].replace("%2B", "+").replace("%2b", "+");
	if !subs.split('+').filter(|s| !s.is_empty()).all(|s| is_name(s, false)) {
		return Some(Err("Invalid subreddit name".to_string()));
	}
	let result = match &caps[2] {
		"about" => subreddit(&subs).await,
		"search" => search(path, Some(&subs)).await,
		sort => feed(path, sort, &subs).await,
	};
	// Handlers show "gated" and "quarantined" as a page to opt in, which doesn't
	// help anymore; after opting in, say so
	Some(result.map_err(|e| match e.as_str() {
		"gated" | "quarantined" if quarantine => format!("r/{subs} is {e}, and Reddit no longer shows it to Redlib"),
		_ => e,
	}))
}

async fn feed(path: &str, sort: &str, subs: &str) -> Result<Value, String> {
	let feed = Feed::new(path, sort, &FEED_SORTS)?;
	let result = match subs.to_lowercase().as_str() {
		"popular" => feed.popular().await,
		"all" => feed.all().await,
		_ if subs.contains('+') => feed.combined(subs.split('+').filter(|s| !s.is_empty()).map(str::to_lowercase).collect()).await,
		_ => feed.subreddit(subs, &feed.after).await,
	};
	result.map(|(posts, after)| listing(posts, &after))
}

fn listing(posts: Vec<Value>, after: &str) -> Value {
	json_value!({ "kind": "Listing", "data": { "children": posts, "after": if after.is_empty() { Value::Null } else { after.into() } } })
}

#[derive(Clone)]
struct Feed {
	sort: String,
	range: Option<String>,
	/// Fullname of the last post of the previous page, or the cursors of a combined feed
	after: String,
}

impl Feed {
	/// A feed with `sort` (one of `sorts`) and the time range and cursor of a JSON API path
	fn new(path: &str, sort: &str, sorts: &[&str]) -> Result<Self, String> {
		Ok(Self {
			sort: choice(sort, sorts, "sort")?.to_uppercase(),
			range: range(path)?.map(|t| t.to_uppercase()),
			after: after(path)?,
		})
	}

	fn variables(&self, after: &str) -> Value {
		let mut variables = json_value!({ "sort": self.sort, "pageSize": PAGE_SIZE });
		if let Some(range) = &self.range {
			variables["range"] = range.as_str().into();
		}
		if !after.is_empty() {
			variables["after"] = to_cursor(after).into();
		}
		variables
	}

	async fn subreddit(&self, name: &str, after: &str) -> Result<(Vec<Value>, String), String> {
		let mut variables = self.variables(after);
		variables["subredditName"] = name.into();
		variables["includeSubredditInPosts"] = true.into();
		let response = query("SubredditFeedElements", SUBREDDIT_FEED_HASH, &variables).await?;
		let feed = &response["data"]["postFeed"];
		match feed["__typename"].as_str() {
			Some("Subreddit") => Ok(page(&feed["elements"], |node| node)),
			// The feed doesn't say why; the info does
			Some("UnavailableSubreddit") => Err(subreddit(name).await.err().unwrap_or_else(|| format!("r/{name} is unavailable"))),
			_ => Err(format!("r/{name} doesn't exist")),
		}
	}

	async fn popular(&self) -> Result<(Vec<Value>, String), String> {
		let response = query("PopularFeedElements", POPULAR_FEED_HASH, &self.variables(&self.after)).await?;
		Ok(page(&response["data"]["popular"]["elements"], |node| &node["postInfo"]))
	}

	/// r/all only gives post IDs, without ads' posts; the posts come from `PostsByIds`.
	async fn all(&self) -> Result<(Vec<Value>, String), String> {
		let mut variables = json_value!({ "sort": self.sort });
		if let Some(range) = &self.range {
			variables["time"] = range.as_str().into();
		}
		let mut after = self.after.clone();
		let mut ids = Vec::new();
		for _ in 0..ALL_MAX_REQUESTS {
			if !after.is_empty() {
				variables["after"] = to_cursor(&after).into();
			}
			let response = query("AllFeedSdui", ALL_FEED_HASH, &variables).await?;
			let elements = &response["data"]["allV3"]["elements"];
			for node in elements["edges"].as_array().into_iter().flatten().map(|edge| &edge["node"]) {
				let is_ad = node["cells"].as_array().into_iter().flatten().any(|cell| cell["__typename"] == "AdMetadataCell");
				if let Some(id) = node["groupId"].as_str().filter(|id| id.starts_with("t3_") && !is_ad) {
					ids.push(id.to_string());
				}
			}
			after = elements["pageInfo"]["endCursor"].as_str().map(from_cursor).unwrap_or_default();
			if ids.len() >= PAGE_SIZE || after.is_empty() {
				break;
			}
		}
		Ok((by_id(ids).await?, after))
	}

	/// Merges the feeds of several subreddits, like the JSON API's `r/a+b`, which
	/// GraphQL lacks. Each page takes posts from the front of each subreddit's
	/// feed, so the cursors (`sub:t3_<id>,…`) say how far each one got.
	async fn combined(&self, subs: Vec<String>) -> Result<(Vec<Value>, String), String> {
		let cursors = self.cursors();
		let tasks: Vec<_> = subs
			.iter()
			.map(|sub| {
				let (feed, sub) = (self.clone(), sub.clone());
				let after = cursors.get(&sub).cloned().unwrap_or_default();
				tokio::spawn(async move { feed.subreddit(&sub, &after).await })
			})
			.collect();
		let mut feeds = Vec::new();
		let mut error = None;
		for (sub, task) in subs.iter().zip(tasks) {
			match task.await.map_err(|e| e.to_string()).and_then(|r| r) {
				// Stickied posts belong to the subreddit's own page
				Ok((posts, _)) => feeds.push((sub.clone(), posts.into_iter().filter(|p| p["data"]["stickied"] != true).collect())),
				Err(e) => {
					debug!("Skipping r/{sub} in combined feed: {e}");
					error.get_or_insert(e);
				}
			}
		}
		if feeds.is_empty() {
			return Err(error.unwrap_or_else(|| "No posts found".to_string()));
		}
		Ok(self.merge(feeds, cursors))
	}

	/// Cursors of a merged feed: `key:<fullname>,…`
	fn cursors(&self) -> HashMap<String, String> {
		self
			.after
			.split(',')
			.filter_map(|c| c.split_once(':'))
			.map(|(key, after)| (key.to_lowercase(), after.to_string()))
			.collect()
	}

	/// Merges feeds into one page, taking items from the front of each feed, so
	/// each feed's cursor says how far it got. Returns the page and its cursors.
	fn merge(&self, feeds: Vec<(String, Vec<Value>)>, mut cursors: HashMap<String, String>) -> (Vec<Value>, String) {
		let keys: Vec<String> = feeds.iter().map(|(key, _)| key.clone()).collect();
		let mut feeds: Vec<(String, VecDeque<(usize, Value)>)> = feeds.into_iter().map(|(key, items)| (key, items.into_iter().enumerate().collect())).collect();
		let sizes: Vec<usize> = feeds.iter().map(|(_, posts)| posts.len()).collect();
		let mut posts = Vec::new();
		while posts.len() < PAGE_SIZE {
			let best = feeds
				.iter()
				.enumerate()
				.filter_map(|(i, (_, posts))| posts.front().map(|(index, post)| (i, rank(&self.sort, post, *index, sizes[i]))))
				.max_by(|a, b| a.1.total_cmp(&b.1));
			let Some((i, _)) = best else { break };
			let (sub, feed) = &mut feeds[i];
			if let Some((_, post)) = feed.pop_front() {
				cursors.insert(sub.clone(), post["data"]["name"].as_str().unwrap_or_default().to_string());
				posts.push(post);
			}
		}

		let after = if posts.is_empty() {
			String::new()
		} else {
			keys
				.iter()
				.filter_map(|key| cursors.get(key).map(|after| format!("{key}:{after}")))
				.collect::<Vec<_>>()
				.join(",")
		};
		(posts, after)
	}

	async fn user_posts(&self, name: &str, after: &str) -> Result<(Vec<Value>, String), String> {
		let mut variables = self.variables(after);
		variables["username"] = name.into();
		let response = query("UserSubmittedPosts", USER_POSTS_HASH, &variables).await?;
		let feed = &response["data"]["postFeed"];
		if feed.is_null() {
			return Err(format!("u/{name} doesn't exist"));
		}
		Ok(page(&feed["posts"], |node| node))
	}

	/// The comments list only has previews; full comments come from `LiveCommentsByIds`.
	async fn user_comments(&self, name: &str, after: &str) -> Result<(Vec<Value>, String), String> {
		let mut variables = self.variables(after);
		variables["name"] = name.into();
		let response = query("SubmittedComments", USER_COMMENTS_HASH, &variables).await?;
		let user = &response["data"]["redditorInfoByName"];
		if user.is_null() {
			return Err(format!("u/{name} doesn't exist"));
		}
		let comments = &user["comments"];
		let previews: Vec<&Value> = comments["edges"]
			.as_array()
			.into_iter()
			.flatten()
			.map(|edge| &edge["node"])
			.filter(|node| node["id"].is_string())
			.collect();
		let after = next_cursor(comments);
		if previews.is_empty() {
			return Ok((Vec::new(), after));
		}

		let ids: Vec<&Value> = previews.iter().map(|node| &node["id"]).collect();
		let response = query(
			"LiveCommentsByIds",
			COMMENTS_BY_IDS_HASH,
			&json_value!({ "ids": ids, "includePremiumAvatarTreatment": false }),
		)
		.await?;
		let full = response["data"]["commentsByIds"].as_array().cloned().unwrap_or_default();
		let comments = previews
			.iter()
			.filter_map(|preview| full.iter().find(|c| c["id"] == preview["id"]).map(|c| convert_comment(c, &preview["postInfo"]["title"])))
			.collect();
		Ok((comments, after))
	}

	/// Posts and comments, merged like a combined feed
	async fn overview(&self, name: &str) -> Result<(Vec<Value>, String), String> {
		let cursors = self.cursors();
		let cursor = |key: &str| cursors.get(key).cloned().unwrap_or_default();
		let (posts, comments) = futures_lite::future::zip(self.user_posts(name, &cursor("posts")), self.user_comments(name, &cursor("comments"))).await;
		let (posts, comments) = (posts?.0, comments?.0);
		Ok(self.merge(vec![("posts".to_string(), posts), ("comments".to_string(), comments)], cursors))
	}
}

/// The next page's cursor of a connection, or empty on the last page
fn next_cursor(connection: &Value) -> String {
	let has_next = connection["pageInfo"]["hasNextPage"] != false;
	connection["pageInfo"]["endCursor"].as_str().filter(|_| has_next).map(from_cursor).unwrap_or_default()
}

/// Order of posts in a combined feed: higher goes first. Each subreddit's feed
/// keeps its own order; sorts without a formula interleave proportionally.
fn rank(sort: &str, post: &Value, index: usize, size: usize) -> f64 {
	let data = &post["data"];
	let score = data["score"].as_f64().unwrap_or_default();
	let created = data["created_utc"].as_f64().unwrap_or_default();
	match sort {
		"NEW" => created,
		"TOP" => score,
		// Reddit's hot ranking
		"HOT" => score.signum() * score.abs().max(1.0).log10() + (created - 1_134_028_003.0) / 45000.0,
		_ => 1.0 - index as f64 / size as f64,
	}
}

/// Posts and the next page's cursor of a feed's `elements`. `post` picks the
/// post from a node; nodes that aren't posts (recommendations) are skipped.
fn page<'a>(elements: &'a Value, post: impl Fn(&'a Value) -> &'a Value) -> (Vec<Value>, String) {
	let posts = elements["edges"]
		.as_array()
		.into_iter()
		.flatten()
		.map(|edge| post(&edge["node"]))
		.filter(|node| node["id"].as_str().is_some_and(|id| id.starts_with("t3_")))
		.map(convert)
		.collect();
	(posts, next_cursor(elements))
}

async fn by_id(ids: Vec<String>) -> Result<Vec<Value>, String> {
	if ids.is_empty() {
		return Ok(Vec::new());
	}
	let variables = json_value!({ "ids": ids, "includePostContentPostHint": true, "includePostContentThumbnailEnabled": true });
	let response = query("PostsByIds", POSTS_BY_IDS_HASH, &variables).await?;
	Ok(
		response["data"]["postsInfoByIds"]
			.as_array()
			.into_iter()
			.flatten()
			.filter(|node| node["id"].is_string())
			.map(convert)
			.collect(),
	)
}

/// Why a subreddit is unavailable, as the JSON API's errors that handlers expect
fn unavailable(sub: &Value, name: &str) -> String {
	match sub["forbiddenReason"].as_str().map(str::to_lowercase).as_deref() {
		Some(reason @ ("gated" | "quarantined" | "private" | "banned")) => reason.to_string(),
		_ => format!("r/{name} is unavailable"),
	}
}

/// Subreddit info as the JSON API's `about`
async fn subreddit(name: &str) -> Result<Value, String> {
	let variables = json_value!({ "subredditName": name, "includeDevvitData": false, "includeRecapFields": false });
	let response = query("SubredditInfoByName", SUBREDDIT_INFO_HASH, &variables).await?;
	let sub = &response["data"]["subredditInfoByName"];
	match sub["__typename"].as_str() {
		Some("Subreddit") => {}
		Some("UnavailableSubreddit") => return Err(unavailable(sub, name)),
		_ => return Err(format!("r/{name} doesn't exist")),
	}
	Ok(json_value!({
		"kind": "t5",
		"data": {
			"display_name": sub["name"],
			"title": sub["title"],
			"public_description": sub["publicDescriptionText"],
			"description_html": format!("<div class=\"md\">{}</div>", content_html(&sub["description"])),
			"subscribers": sub["subscribersCount"].as_f64().map(|n| n as u64),
			// Reddit no longer counts users online
			"accounts_active": sub["communityStats"]["weeklyActiveUsersCount"],
			"community_icon": sub["styles"]["icon"],
			"icon_img": sub["styles"]["legacyIcon"]["url"],
			"wiki_enabled": sub["wiki"]["indexPage"]["status"] == "VALID",
			"over18": sub["isNsfw"] == true,
		}
	}))
}

/// User info as the JSON API's `about`
async fn user(name: &str) -> Result<Value, String> {
	let response = query("UserProfileDetails", USER_HASH, &json_value!({ "username": name })).await?;
	let user = &response["data"]["redditorInfoByName"];
	if user["__typename"] != "Redditor" {
		return Err(format!("u/{name} doesn't exist"));
	}
	let profile = &user["profileInfo"];
	Ok(json_value!({
		"kind": "t2",
		"data": {
			"name": user["name"],
			"created": timestamp(&profile["createdAt"]),
			"total_karma": user["karma"]["total"],
			"subreddit": {
				"title": profile["title"],
				"icon_img": user["icon"]["url"],
				"banner_img": profile["styles"]["profileBanner"],
				"public_description": profile["publicDescriptionText"],
				"over_18": profile["isNsfw"] == true,
			}
		}
	}))
}

/// A wiki page as the JSON API's, without `content_html` if it doesn't exist
async fn wiki(sub: &str, page: &str) -> Result<Value, String> {
	let variables = json_value!({
		"subredditName": sub,
		"pageName": page,
		"includePage": true,
		"includePageContent": true,
		"includeMediaFragment": true,
		"includeIsRevisable": false,
		"includeRevisionsCount": false,
		"includePageTitle": true,
	});
	let response = query("SubredditWiki", WIKI_HASH, &variables).await?;
	let info = &response["data"]["subredditInfoByName"];
	match info["__typename"].as_str() {
		Some("Subreddit") => {}
		Some("UnavailableSubreddit") => return Err(subreddit(sub).await.err().unwrap_or_else(|| format!("r/{sub} is unavailable"))),
		_ => return Err(format!("r/{sub} doesn't exist")),
	}
	let page = &info["wiki"]["page"];
	let content = &page["content"];
	let html = (page["status"] == "VALID").then(|| format!("<div class=\"md wiki\">{}</div>", content_html(content)));
	Ok(json_value!({ "kind": "wikipage", "data": { "content_html": html } }))
}

async fn duplicate_ids(id: &str) -> Result<Vec<String>, String> {
	let response = query("CreatorStats", DUPLICATES_HASH, &json_value!({ "postId": format!("t3_{id}") })).await?;
	let others = response["data"]["postInfoById"]["otherDiscussions"]["edges"].as_array().cloned().unwrap_or_default();
	Ok(others.iter().filter_map(|edge| edge["node"]["id"].as_str().map(str::to_string)).collect())
}

/// Number of duplicates of a post, 0 if unknown
pub(crate) async fn duplicate_count(id: &str) -> usize {
	duplicate_ids(id).await.map_or(0, |ids| ids.len())
}

/// A post and its duplicates (other posts of the same link), as the JSON API's
/// `[post, duplicates]`. Duplicates come in one page, sorted here.
async fn duplicates(id: &str, sort: &str) -> Result<Value, String> {
	// Only a few fields each; the full posts come from `PostsByIds`
	let mut ids = vec![format!("t3_{id}")];
	ids.extend(duplicate_ids(id).await?);
	let mut posts = by_id(ids).await?;
	if posts.first().is_none_or(|post| post["data"]["id"] != id) {
		return Err("Post not found".to_string());
	}
	let mut duplicates = posts.split_off(1);
	posts[0]["data"]["num_duplicates"] = duplicates.len().into();
	let key = if sort == "new" { "created_utc" } else { "num_comments" };
	duplicates.sort_by(|a, b| b["data"][key].as_f64().unwrap_or_default().total_cmp(&a["data"][key].as_f64().unwrap_or_default()));
	Ok(json_value!([listing(posts, ""), listing(duplicates, "")]))
}

/// Post search, everywhere or (`restrict_sr`) in subreddit `sub`
async fn search(path: &str, sub: Option<&str>) -> Result<Value, String> {
	let text = param(path, "q").unwrap_or_default();
	let restrict = param(path, "restrict_sr").is_some_and(|r| !r.is_empty());
	let text = match sub {
		Some(sub) if restrict => format!("subreddit:{sub} {text}"),
		_ => text,
	};
	let range = range(path)?.unwrap_or_else(|| "all".to_string());
	let sort = choice(
		&param(path, "sort").filter(|s| !s.is_empty()).unwrap_or_else(|| "relevance".to_string()),
		&SEARCH_SORTS,
		"sort",
	)?;
	let nsfw = if param(path, "include_over_18").is_some() { "1" } else { "0" };
	let mut variables = json_value!({
		"query": text,
		"sort": sort.to_uppercase(),
		"filters": [{ "key": "time_range", "value": range }, { "key": "nsfw", "value": nsfw }],
		"pageSize": PAGE_SIZE,
		"productSurface": "android",
	});
	let after = after(path)?;
	if !after.is_empty() {
		variables["afterCursor"] = after.into();
	}
	let response = query("SearchPosts", SEARCH_POSTS_HASH, &variables).await?;
	let (posts, after) = page(&response["data"]["search"]["general"]["posts"], |node| node);
	Ok(listing(posts, &after))
}

/// Subreddit search as the JSON API's listing of `t5` items
async fn search_subreddits(path: &str) -> Result<Value, String> {
	let limit = param(path, "limit").and_then(|l| l.parse::<u32>().ok()).unwrap_or(3);
	let variables = json_value!({ "query": param(path, "q").unwrap_or_default(), "pageSize": limit, "productSurface": "android" });
	let response = query("SearchCommunities", SEARCH_SUBREDDITS_HASH, &variables).await?;
	let subreddits = response["data"]["search"]["general"]["communities"]["edges"]
		.as_array()
		.into_iter()
		.flatten()
		.map(|edge| &edge["node"])
		.filter(|sub| sub["name"].is_string())
		.map(|sub| {
			json_value!({
				"kind": "t5",
				"data": {
					"display_name": sub["name"],
					"url": sub["path"],
					"community_icon": sub["styles"]["icon"],
					"icon_img": sub["styles"]["legacyIcon"]["url"],
					"public_description": sub["publicDescriptionText"],
					"subscribers": sub["subscribersCount"],
				}
			})
		})
		.collect();
	Ok(listing(subreddits, ""))
}

/// Converts a GraphQL comment, from `LiveCommentsByIds`, to an item of a user's
/// comments in the JSON API.
fn convert_comment(node: &Value, link_title: &Value) -> Value {
	let fullname = node["id"].as_str().unwrap_or_default();
	let content = &node["content"];
	let permalink = node["permalink"].as_str().unwrap_or_default();
	// `/r/<sub>/comments/…`, or `/user/<name>/comments/…` for profile posts
	let mut segments = permalink.split('/').skip(1);
	let subreddit = match (segments.next(), segments.next()) {
		(Some("r"), Some(sub)) => sub.to_string(),
		(Some("user"), Some(name)) => format!("u_{name}"),
		_ => String::new(),
	};
	let author = &node["authorInfo"];
	json_value!({
		"kind": "t1",
		"data": {
			"id": fullname.strip_prefix("t1_").unwrap_or(fullname),
			"name": fullname,
			"author": author["name"].as_str().or(author["attributes"]["redditHandleInfo"]["username"].as_str()).unwrap_or("[deleted]"),
			"body": content["markdown"],
			"body_html": format!("<div class=\"md\">{}</div>", content_html(content)),
			"score": node["score"].as_i64().unwrap_or_default(),
			"created_utc": timestamp(&node["createdAt"]).unwrap_or_default(),
			"permalink": permalink,
			"link_title": link_title,
			"subreddit": subreddit,
			"distinguished": node["distinguishedAs"].as_str().map(str::to_lowercase),
			"stickied": node["isStickied"] == true,
		}
	})
}

fn is_fullname(cursor: &str) -> bool {
	cursor.starts_with("t3_") || cursor.starts_with("t1_")
}

/// GraphQL's cursors are base64 fullnames, except search's (offsets), which
/// stay as they are
fn to_cursor(after: &str) -> String {
	if is_fullname(after) {
		STANDARD.encode(after)
	} else {
		after.to_string()
	}
}

fn from_cursor(cursor: &str) -> String {
	STANDARD
		.decode(cursor)
		.ok()
		.and_then(|c| String::from_utf8(c).ok())
		.filter(|c| is_fullname(c))
		.unwrap_or_else(|| cursor.to_string())
}

/// Converts a GraphQL post to an item of the JSON API.
fn convert(node: &Value) -> Value {
	let fullname = node["id"].as_str().unwrap_or_default();
	let content = &node["content"];
	let (selftext, selftext_html) = if content.is_object() {
		(content["markdown"].clone(), format!("<div class=\"md\">{}</div>", content_html(content)))
	} else {
		(Value::Null, String::new())
	};
	let author = &node["authorInfo"];
	let author_flair = &node["authorFlair"];
	let (author_flair_type, author_flair_richtext) = flair_parts(author_flair);
	let flair = &node["flair"];
	let (link_flair_type, link_flair_richtext) = flair_parts(flair);
	let domain = node["domain"].as_str().unwrap_or_default();
	let is_self = node["isSelfPost"] == true;

	let mut data = json_value!({
		"id": fullname.strip_prefix("t3_").unwrap_or(fullname),
		"name": fullname,
		"title": node["title"].as_str().or(node["postTitle"].as_str()),
		"subreddit": node["subreddit"]["name"],
		"author": author["name"].as_str().or(author["displayName"].as_str()).unwrap_or("[deleted]"),
		"author_flair_type": author_flair_type,
		"author_flair_richtext": author_flair_richtext,
		"author_flair_text": author_flair["text"],
		"author_flair_background_color": author_flair["template"]["backgroundColor"],
		"author_flair_text_color": author_flair["textColor"].as_str().map(str::to_lowercase),
		"link_flair_type": link_flair_type,
		"link_flair_richtext": link_flair_richtext,
		"link_flair_text": flair["text"],
		"link_flair_background_color": flair["template"]["backgroundColor"],
		"link_flair_text_color": flair["textColor"].as_str().map(str::to_lowercase),
		"distinguished": node["distinguishedAs"].as_str().map(str::to_lowercase),
		"removed_by_category": node["removedByCategory"].as_str().map(str::to_lowercase),
		"selftext": selftext,
		"selftext_html": selftext_html,
		"score": node["score"].as_i64().unwrap_or_default(),
		"hide_score": node["isScoreHidden"] == true,
		"upvote_ratio": node["upvoteRatio"],
		"created_utc": timestamp(&node["createdAt"]).unwrap_or_default(),
		"edited": timestamp(&node["editedAt"]).map_or(Value::Bool(false), Value::from),
		"num_comments": node["commentCount"].as_i64().unwrap_or_default(),
		"over_18": node["isNsfw"] == true,
		"spoiler": node["isSpoiler"] == true,
		"stickied": node["isStickied"] == true,
		"locked": node["isLocked"] == true,
		"archived": node["isArchived"] == true,
		"permalink": node["permalink"],
		"url": node["url"],
		"url_overridden_by_dest": if is_self { Value::Null } else { node["url"].clone() },
		"domain": domain,
		"is_self": is_self,
		"is_reddit_media_domain": domain == "i.redd.it" || domain == "v.redd.it",
		// SELF, IMAGE, HOSTED_VIDEO, RICH_VIDEO, LINK as self, image, hosted:video, …
		"post_hint": node["postHint"].as_str().map(|hint| hint.to_lowercase().replace('_', ":")),
		"thumbnail": node["thumbnail"]["url"].as_str().unwrap_or_default(),
		"thumbnail_width": node["thumbnail"]["dimensions"]["width"],
		"thumbnail_height": node["thumbnail"]["dimensions"]["height"],
		"all_awardings": awards(&node["awardings"]),
	});

	// The comment sort to use when none is chosen, like CONFIDENCE or QA
	data["suggested_sort"] = node["suggestedCommentSort"].as_str().map(str::to_lowercase).into();

	let media = &node["media"];
	let still = &media["still"]["source"];
	if still.is_object() {
		let mut image = json_value!({ "source": source(still), "variants": {} });
		let mp4 = &media["animated"]["mp4_source"];
		if mp4.is_object() {
			image["variants"]["mp4"] = json_value!({ "source": source(mp4) });
		}
		data["preview"] = json_value!({ "images": [image] });
	}
	let streaming = &media["streaming"];
	if streaming.is_object() {
		// The muxed MP4 has sound; the JSON API's fallback_url had none
		let mp4 = media["packagedMedia"]["muxedMp4s"]["recommended"]["url"].as_str().or(media["download"]["url"].as_str());
		// Reddit's copies of external videos (Streamable, …) give the MP4 as `hlsUrl` too
		let hls = streaming["hlsUrl"].as_str().filter(|url| url.contains(".m3u8"));
		data["secure_media"] = json_value!({ "reddit_video": { "fallback_url": mp4, "hls_url": hls, "is_gif": streaming["isGif"] == true } });
	}

	if let Some(items) = node["gallery"]["items"].as_array() {
		let items: Vec<&Value> = items.iter().filter(|item| item["media"]["id"].is_string()).collect();
		data["is_gallery"] = true.into();
		data["gallery_data"] = json_value!({ "items": items.iter().map(|item| json_value!({ "media_id": item["media"]["id"], "caption": item["caption"], "outbound_url": item["outboundUrl"] })).collect::<Vec<_>>() });
		data["media_metadata"] = items
			.iter()
			.filter_map(|item| {
				let media = &item["media"];
				let image = json_value!({ "u": media["url"], "gif": media["url"], "x": media["width"], "y": media["height"] });
				Some((media["id"].as_str()?.to_string(), json_value!({ "m": media["mimetype"], "s": image })))
			})
			.collect::<serde_json::Map<_, _>>()
			.into();
	}

	let poll = &node["poll"];
	if poll.is_object() {
		let options: Vec<Value> = poll["options"]
			.as_array()
			.into_iter()
			.flatten()
			.map(|option| json_value!({ "id": option["id"], "text": option["text"], "vote_count": option["voteCount"] }))
			.collect();
		data["poll_data"] = json_value!({
			"options": options,
			"total_vote_count": poll["totalVoteCount"],
			// In milliseconds, like the JSON API's
			"voting_end_timestamp": timestamp(&poll["votingEndsAt"]).map(|t| t * 1000.0),
		});
	}

	let crosspost = &node["crosspostRoot"]["post"];
	if crosspost["id"].is_string() {
		data["crosspost_parent_list"] = json_value!([convert(crosspost)["data"]]);
	}

	json_value!({ "kind": "t3", "data": data })
}

fn source(media: &Value) -> Value {
	json_value!({ "url": media["url"], "width": media["dimensions"]["width"], "height": media["dimensions"]["height"] })
}

#[cfg(test)]
mod tests {
	use super::*;

	#[test]
	fn test_cursor() {
		assert_eq!(to_cursor("t3_1xhmntk"), "dDNfMXhobW50aw==");
		assert_eq!(from_cursor("dDNfMXhobW50aw=="), "t3_1xhmntk");
		// Anything else passes through
		assert_eq!(from_cursor("abc"), "abc");
		assert_eq!(to_cursor("abc"), "abc");
	}

	#[test]
	fn test_checks() {
		assert_eq!(choice("Top", &FEED_SORTS, "sort"), Ok("top".to_string()));
		assert!(choice("TOP\"}", &FEED_SORTS, "sort").is_err());
		assert_eq!(range("?t=week"), Ok(Some("week".to_string())));
		assert_eq!(range("?t="), Ok(None));
		assert!(range("?t=decade").is_err());
		assert!(after(&format!("?after={}", "a".repeat(MAX_CURSOR_LEN + 1))).is_err());
		assert!(is_name("rust", false) && is_name("u_spez", false) && is_name("some-user", true));
		assert!(!is_name("some-user", false) && !is_name("../x", true) && !is_name("", false));
	}

	#[test]
	fn test_rank() {
		let post = |score: i64, created: f64| json_value!({ "data": { "score": score, "created_utc": created } });
		// A day newer outweighs 10 times the score
		assert!(rank("HOT", &post(100, 1_800_000_000.0 + 86400.0), 0, 1) > rank("HOT", &post(1000, 1_800_000_000.0), 0, 1));
		assert!(rank("HOT", &post(-10, 1_800_000_000.0), 0, 1) < rank("HOT", &post(0, 1_800_000_000.0), 0, 1));
		assert!(rank("RISING", &post(0, 0.0), 1, 4) < rank("RISING", &post(0, 0.0), 0, 2));
	}
}
