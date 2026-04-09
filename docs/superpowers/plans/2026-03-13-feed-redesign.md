# Feed & Comment UX Redesign Implementation Plan

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Redesign Redlib's feed layouts as density variants with large thumbnails, auto-collapse deep comments, and increase posts per page.

**Architecture:** The three layout modes (card/clean/compact) become density variants of one flexbox-based post layout. Comment depth tracking is added at parse time in Rust. Two new user preferences (post_count, collapse_depth) integrate with the existing cookie/config system.

**Tech Stack:** Rust (Hyper, Askama templates), CSS3, HTML5

**Spec:** `docs/superpowers/specs/2026-03-13-feed-redesign-design.md`

---

## Chunk 1: Backend — New Preferences & Config

### Task 1: Add new fields to Config struct

**Files:**
- Modify: `src/config.rs:18-111` (Config struct)
- Modify: `src/config.rs:134-159` (Config::load)
- Modify: `src/config.rs:163-191` (get_setting_from_config)

- [ ] **Step 1: Add fields to Config struct**

In `src/config.rs`, add two new fields to the `Config` struct after line 110 (before the closing `}`):

```rust
	#[serde(rename = "REDLIB_DEFAULT_POST_COUNT")]
	pub(crate) default_post_count: Option<String>,

	#[serde(rename = "REDLIB_DEFAULT_COLLAPSE_DEPTH")]
	pub(crate) default_collapse_depth: Option<String>,
```

- [ ] **Step 2: Add parse calls in Config::load()**

In `Config::load()`, add after line 158 (`default_remove_default_feeds`):

```rust
			default_post_count: parse("REDLIB_DEFAULT_POST_COUNT"),
			default_collapse_depth: parse("REDLIB_DEFAULT_COLLAPSE_DEPTH"),
```

- [ ] **Step 3: Add match arms in get_setting_from_config()**

In `get_setting_from_config()`, add before the `_ => None` arm (line 189):

```rust
		"REDLIB_DEFAULT_POST_COUNT" => config.default_post_count.clone(),
		"REDLIB_DEFAULT_COLLAPSE_DEPTH" => config.default_collapse_depth.clone(),
```

- [ ] **Step 4: Build to verify compilation**

Run: `cargo build 2>&1 | tail -5`
Expected: successful build (no errors)

- [ ] **Step 5: Commit**

```bash
git add src/config.rs
git commit -m "feat: add post_count and collapse_depth config fields"
```

---

### Task 2: Add new fields to Preferences struct and PREFS array

**Files:**
- Modify: `src/utils.rs:623-673` (Preferences struct)
- Modify: `src/utils.rs:698-732` (Preferences::new)
- Modify: `src/settings.rs:27-47` (PREFS array)

- [ ] **Step 1: Bump revision and add fields to Preferences struct**

In `src/utils.rs`, first bump the struct-level revision from `1` to `2`:

Change line 624:
```rust
#[revisioned(revision = 2)]
```

Then add two new fields to `Preferences` after line 672 (`remove_default_feeds`), before the closing `}`:

```rust
	#[revision(start = 2)]
	pub post_count: String,
	#[revision(start = 2)]
	pub collapse_depth: String,
```

**Important:** These MUST use `start = 2` (not `start = 1`) so that old exported settings (bincode-serialized at revision 1) can still be deserialized — the revisioned crate will use `String::default()` (empty string) for these fields when deserializing revision-1 data.

- [ ] **Step 2: Add setting reads in Preferences::new()**

In `src/utils.rs`, in `Preferences::new()`, add after line 730 (`remove_default_feeds`), before the closing `}`:

```rust
			post_count: setting(req, "post_count"),
			collapse_depth: setting(req, "collapse_depth"),
```

- [ ] **Step 3: Update PREFS array in settings.rs**

In `src/settings.rs`, change line 27 from:
```rust
const PREFS: [&str; 19] = [
```
to:
```rust
const PREFS: [&str; 21] = [
```

And add after line 46 (`"remove_default_feeds"`):
```rust
	"post_count",
	"collapse_depth",
```

- [ ] **Step 4: Build to verify compilation**

Run: `cargo build 2>&1 | tail -5`
Expected: successful build

- [ ] **Step 5: Commit**

```bash
git add src/utils.rs src/settings.rs
git commit -m "feat: add post_count and collapse_depth to Preferences"
```

---

### Task 3: Add depth and descendant_count to Comment struct

**Files:**
- Modify: `src/utils.rs:469-492` (Comment struct)
- Modify: `src/post.rs:117-134` (parse_comments)
- Modify: `src/post.rs:136-163` (query_comments)
- Modify: `src/post.rs:164-257` (build_comment)

- [ ] **Step 1: Add fields to Comment struct**

In `src/utils.rs`, add three new fields to `Comment` after line 491 (`prefs`), before the closing `}`:

```rust
	pub depth: u32,
	pub descendant_count: u32,
	pub auto_collapsed: bool,
```

- [ ] **Step 2: Modify parse_comments to accept and pass depth**

In `src/post.rs`, change `parse_comments` signature (line 117) to:

```rust
fn parse_comments(json: &serde_json::Value, post_link: &str, post_author: &str, highlighted_comment: &str, filters: &HashSet<String>, req: &Request<Body>, depth: u32) -> Vec<Comment> {
```

Change the recursive call (line 127) to pass `depth + 1`:
```rust
				parse_comments(&data["replies"], post_link, post_author, highlighted_comment, filters, req, depth + 1)
```

Change the build_comment call (line 131) to pass `depth`:
```rust
			build_comment(&comment, data, replies, post_link, post_author, highlighted_comment, filters, req, depth)
```

- [ ] **Step 3: Modify query_comments to accept and pass depth**

In `src/post.rs`, change `query_comments` signature (line 136) to add `depth: u32` parameter:

```rust
fn query_comments(
	json: &serde_json::Value,
	post_link: &str,
	post_author: &str,
	highlighted_comment: &str,
	filters: &HashSet<String>,
	query: &str,
	req: &Request<Body>,
	depth: u32,
) -> Vec<Comment> {
```

Change the recursive call (line 153) to pass `depth + 1`:
```rust
			results.append(&mut query_comments(&data["replies"], post_link, post_author, highlighted_comment, filters, query, req, depth + 1));
```

Change the build_comment call (line 156) to pass `depth`:
```rust
		let c = build_comment(&comment, data, Vec::new(), post_link, post_author, highlighted_comment, filters, req, depth);
```

- [ ] **Step 4: Modify build_comment to accept depth and compute descendant_count**

In `src/post.rs`, change `build_comment` signature (line 165) to add `depth: u32`:

```rust
fn build_comment(
	comment: &serde_json::Value,
	data: &serde_json::Value,
	replies: Vec<Comment>,
	post_link: &str,
	post_author: &str,
	highlighted_comment: &str,
	filters: &HashSet<String>,
	req: &Request<Body>,
	depth: u32,
) -> Comment {
```

Add descendant count and auto_collapsed computation before the `Comment {` construction (before line 232). Reuse the `Preferences` instance instead of calling `Preferences::new(req)` twice:

```rust
	let descendant_count: u32 = replies.iter().map(|r| 1 + r.descendant_count).sum();
	let prefs = Preferences::new(req);
	let collapse_depth: u32 = if prefs.collapse_depth == "off" || prefs.collapse_depth.is_empty() {
		u32::MAX
	} else {
		prefs.collapse_depth.parse().unwrap_or(2)
	};
	let auto_collapsed = depth >= collapse_depth;
```

Add the three new fields to the `Comment` struct literal, and change the existing `prefs` line to use the already-constructed instance (replace `prefs: Preferences::new(req)` with just `prefs`):

```rust
		prefs,
		depth,
		descendant_count,
		auto_collapsed,
```

- [ ] **Step 5: Update all callers of parse_comments and query_comments**

Search for all call sites. In `src/post.rs`, the `item` function calls `parse_comments` and `query_comments`. Update those calls to pass `depth: 0`:

Find the call to `parse_comments` in the `item` function (around line 85-100) and add `, 0` as the last argument.

Find the call to `query_comments` in the `item` function and add `, 0` as the last argument.

Run: `cargo build 2>&1 | head -30` to find any remaining call sites that need updating.

- [ ] **Step 6: Build to verify compilation**

Run: `cargo build 2>&1 | tail -5`
Expected: successful build

- [ ] **Step 7: Commit**

```bash
git add src/utils.rs src/post.rs
git commit -m "feat: add depth tracking and descendant_count to comments"
```

---

### Task 4: Add limit parameter to subreddit API requests

**Files:**
- Modify: `src/subreddit.rs:135-144` (API URL construction)

- [ ] **Step 1: Read post_count and add limit to API URL**

In `src/subreddit.rs`, after line 135 (`let mut params = String::from("&raw_json=1");`), add:

```rust
	// Read post count preference, validate and clamp to 1-100
	let post_count = setting(&req, "post_count");
	let limit: u32 = post_count.parse().unwrap_or(50).clamp(1, 100);
	params.push_str(&format!("&limit={limit}"));
```

- [ ] **Step 2: Also update user.rs and search.rs API paths**

Check `src/user.rs` and `src/search.rs` for similar API URL construction. Add the same `limit` parameter logic where `Post::fetch` is called with a constructed path.

In `src/user.rs`, find where the path is built (before `Post::fetch` call around line 83) and add `&limit={limit}` to the URL params.

In `src/search.rs`, find where the path is built (before `Post::fetch` call around line 132) and add `&limit={limit}` to the URL params.

- [ ] **Step 3: Build to verify compilation**

Run: `cargo build 2>&1 | tail -5`
Expected: successful build

- [ ] **Step 4: Commit**

```bash
git add src/subreddit.rs src/user.rs src/search.rs
git commit -m "feat: add configurable limit parameter to API requests (default 50)"
```

---

## Chunk 2: Templates — Settings UI & Comment Collapse

### Task 5: Add new preference dropdowns to settings page

**Files:**
- Modify: `templates/settings.html` (add dropdowns)

- [ ] **Step 1: Read templates/settings.html to find insertion point**

Read the settings template to find where the layout dropdown is (around line 42-46) and where content preferences are grouped.

- [ ] **Step 2: Add Posts per page dropdown**

After the existing layout dropdown group, add:

```html
			<div class="prefs-group">
				<label for="post_count">Posts per page:</label>
				<select name="post_count" id="post_count">
					{% call utils::options(prefs.post_count, ["25", "50", "100"], "50") %}
				</select>
			</div>
```

- [ ] **Step 3: Add Comment collapse depth dropdown**

After the comment_sort dropdown group, add:

```html
			<div class="prefs-group">
				<label for="collapse_depth">Collapse comments at depth:</label>
				<select name="collapse_depth" id="collapse_depth">
					{% call utils::options(prefs.collapse_depth, ["2", "3", "5", "10", "off"], "2") %}
				</select>
			</div>
```

- [ ] **Step 4: Build to verify template compiles**

Run: `cargo build 2>&1 | tail -5`
Expected: successful build

- [ ] **Step 5: Commit**

```bash
git add templates/settings.html
git commit -m "feat: add posts per page and collapse depth settings UI"
```

---

### Task 6: Update comment.html for depth-based collapsing

**Files:**
- Modify: `templates/comment.html` (collapse logic, depth colors, pill)

- [ ] **Step 1: Read the current comment.html**

Read `templates/comment.html` to understand the full current structure.

- [ ] **Step 2: Update the `<details>` open logic**

The `auto_collapsed` boolean was already computed in Rust (Task 3) and is a field on the Comment struct. Use it directly in the template.

Change the `<details>` line (line 17) from:
```html
<details class="comment_right" {% if !collapsed || highlighted %}open{% endif %}>
```
to:
```html
<details class="comment_right" {% if (!collapsed && !auto_collapsed) || highlighted %}open{% endif %}>
```

This uses the pre-computed `auto_collapsed` field — no Askama method calls needed.

- [ ] **Step 3: Add descendant count pill to the summary**

In `comment.html`, modify the `<summary>` element to show the descendant count when auto-collapsed. After the existing summary content, add the collapse pill:

```html
{% if auto_collapsed && descendant_count > 0 %}
<span class="collapse_pill">▸ {{ descendant_count }} more replies</span>
{% endif %}
```

- [ ] **Step 4: Add depth-based color class to collapse bar**

In `comment.html`, update the `.line` element (line 15) to include a depth class:

```html
<div class="line depth_{{ depth.min(2) }}"></div>
```

This maps depths 0, 1, 2+ to classes `depth_0`, `depth_1`, `depth_2` for CSS color coding.

- [ ] **Step 5: Build to verify template compiles**

Run: `cargo build 2>&1 | tail -5`
Expected: successful build. If Askama rejects the template syntax, adjust accordingly.

- [ ] **Step 6: Commit**

```bash
git add templates/comment.html src/utils.rs src/post.rs
git commit -m "feat: depth-based comment auto-collapse with pill indicator"
```

---

## Chunk 3: CSS — Feed Layout Refactor

### Task 7: Restructure post_in_list template for flexbox layout

**Files:**
- Modify: `templates/utils.html:220-323` (post_in_list macro)

- [ ] **Step 1: Read the current post_in_list macro**

Read `templates/utils.html` lines 220-323 to understand the full current macro structure.

- [ ] **Step 2: Rewrite the post_in_list macro**

Replace the grid-based layout with a flexbox layout. The new structure must preserve all existing functionality.

**Critical things to preserve from the current template (lines 220-323):**

1. **`u_` community prefix** (lines 224-229): The current template checks `post.community.starts_with("u_")` to show `u/username` instead of `r/u_username`. This MUST be preserved:
   ```html
   {% if post.community.starts_with("u_") %}
   <a class="post_subreddit" href="/u/{{ post.community.strip_prefix("u_").unwrap() }}">u/{{ post.community.strip_prefix("u_").unwrap() }}</a>
   {% else %}
   <a class="post_subreddit" href="/r/{{ post.community }}">r/{{ post.community }}</a>
   {% endif %}
   ```

2. **`stickied` class on outer div** (line 222): `{% if post.flags.stickied %} stickied{% endif %}` — without this, stickied posts lose their green accent.

3. **`post_blurred` class on outer `.post` div** (not on thumbnail div): The existing CSS uses `.post_blurred .post_thumbnail *` descendant selector. Keep blur class on the outer div.

4. **Poll rendering** (line 317): `{% call poll(post) %}` must be preserved for poll-type posts.

5. **Flair, awards, NSFW/spoiler badges**: Copy the exact conditional blocks from the current template.

6. **Self posts**: Must render a placeholder icon in thumbnail slot (reuse existing SVG from lines 289-292).

The new structure for each post:

```html
<div class="post {% if post.flags.stickied %} stickied{% endif %}{% if post.flags.nsfw && prefs.blur_nsfw == "on" %} post_blurred{% endif %}" id="{{ post.id }}">
	<div class="post_content">
		<div class="post_header">
			<!-- u_ prefix handling as above -->
			<span class="dot">&middot;</span>
			<span class="post_rel_time">{{ post.rel_time }}</span>
			{% if prefs.layout.is_empty() || prefs.layout == "card" %}
			<span class="dot">&middot;</span>
			<a class="post_author" href="/u/{{ post.author.name }}">u/{{ post.author.name }}</a>
			{% endif %}
			<!-- PRESERVE: flair, stickied pin, nsfw/spoiler badges from current template -->
		</div>
		<a class="post_title" href="{{ post.permalink }}">{{ post.title }}</a>
		{% if (prefs.layout.is_empty() || prefs.layout == "card") && !post.body.is_empty() %}
		<div class="post_body post_preview">{{ post.body|safe }}</div>
		{% endif %}
		{% call poll(post) %}
		<div class="post_footer">
			<!-- PRESERVE: score visibility (hide_score pref), comment count, share, domain -->
		</div>
	</div>
	<div class="post_thumbnail">
		{% if !post.thumbnail.url.is_empty() %}
		<img loading="lazy" src="{{ post.thumbnail.url }}" alt="thumbnail">
		{% else %}
		<!-- placeholder SVG icon (reuse existing from lines 289-292) -->
		{% endif %}
	</div>
</div>
```

Key changes from current:
- Remove CSS grid areas (post_score, post_header, etc.)
- Use `.post_content` (flex: 1) + `.post_thumbnail` (flex-shrink: 0) flexbox row
- Move score into footer row instead of separate column
- Remove inline media rendering for card mode in list view (images/videos only on post detail page)
- Always render thumbnail slot (with placeholder for self/text posts)
- Keep `post_blurred` on outer `.post` div (not on thumbnail)
- Preserve `stickied` class, `u_` prefix handling, poll rendering, all badges

- [ ] **Step 3: Build to verify template compiles**

Run: `cargo build 2>&1 | tail -5`

- [ ] **Step 4: Commit**

```bash
git add templates/utils.html
git commit -m "feat: restructure post_in_list from grid to flexbox layout"
```

---

### Task 8: Rewrite post CSS for density-based layouts

**Files:**
- Modify: `static/style.css` (post layout rules, density variants, compact rules)

- [ ] **Step 1: Read the current post grid CSS**

Read `static/style.css` lines 971-1000 (`.post` grid layout) and lines 1578-1637 (`.compact` rules).

- [ ] **Step 2: Replace .post grid with flexbox base layout**

Replace the `.post` grid rules (around lines 971-986) with:

```css
.post {
	display: flex;
	gap: var(--post-gap, 12px);
	padding: var(--post-padding, 12px);
	align-items: flex-start;
	cursor: pointer;
	background: var(--post);
	border: var(--panel-border);
	border-radius: 5px;
	margin-bottom: 8px;
}

.post:hover {
	background: var(--highlighted);
}

.post_content {
	flex: 1;
	min-width: 0;
}

.post_thumbnail {
	flex-shrink: 0;
	width: var(--thumb-width, 120px);
	height: var(--thumb-height, 90px);
	border-radius: 5px;
	overflow: hidden;
}

.post_thumbnail img {
	width: 100%;
	height: 100%;
	object-fit: cover;
	border-radius: 5px;
}
```

- [ ] **Step 3: Add density variant CSS variables**

Add CSS for each layout density. These use the body class set in `base.html`:

```css
/* Card (Spacious) - default */
body, body.card {
	--thumb-width: 120px;
	--thumb-height: 90px;
	--post-padding: 12px;
	--post-gap: 12px;
}

/* Clean (Normal) */
body.clean {
	--thumb-width: 100px;
	--thumb-height: 75px;
	--post-padding: 10px 0;
	--post-gap: 10px;
}

body.clean .post {
	background: none;
	border: none;
	border-radius: 0;
	border-bottom: var(--panel-border);
	margin-bottom: 0;
}

/* Compact (Dense) */
body.compact {
	--thumb-width: 60px;
	--thumb-height: 45px;
	--post-padding: 6px 8px;
	--post-gap: 8px;
}

body.compact .post {
	background: none;
	border: none;
	border-radius: 0;
	border-bottom: var(--panel-border);
	margin-bottom: 0;
	align-items: center;
}

body.compact .post_title {
	font-size: 12px;
	white-space: nowrap;
	overflow: hidden;
	text-overflow: ellipsis;
}

body.compact .post_body.post_preview {
	display: none;
}

body.compact .post_thumbnail {
	order: -1;
	border-radius: 3px;
}
```

- [ ] **Step 4: Remove old .compact rules**

Remove the old `.compact` CSS rules (lines 1578-1637) that referenced the grid layout, as they're replaced by the new density variant CSS above.

- [ ] **Step 5: Update NSFW blur selectors for new DOM structure**

Ensure the blur rules (around lines 1144-1162) work with the new flexbox structure. Update selectors if needed:

```css
.post_blurred .post_thumbnail img {
	filter: blur(0.3rem);
}
.post_blurred .post_thumbnail:hover img {
	filter: none;
}
```

- [ ] **Step 6: Build and visually verify**

Run: `cargo build 2>&1 | tail -5`
Expected: successful build

- [ ] **Step 7: Commit**

```bash
git add static/style.css
git commit -m "feat: density-based post layout CSS (card/clean/compact)"
```

---

### Task 9: Add comment collapse bar styles

**Files:**
- Modify: `static/style.css` (collapse bar, depth colors, pill)

- [ ] **Step 1: Update collapse bar (.line) styles**

Replace the `.line` rules (around lines 1537-1541) with:

```css
.line {
	width: 3px;
	height: 100%;
	background: var(--foreground);
	border-radius: 2px;
	cursor: pointer;
}

.line.depth_0 {
	background: var(--accent);
}

.line.depth_1 {
	background: #44aacc;
}

.line.depth_2 {
	background: #888;
}
```

- [ ] **Step 2: Add collapse pill styles**

Add new CSS for the "N more replies" pill:

```css
.collapse_pill {
	display: inline-flex;
	align-items: center;
	gap: 6px;
	background: var(--outside);
	border: var(--panel-border);
	border-radius: 4px;
	padding: 8px 12px;
	color: var(--visited);
	font-size: 12px;
	cursor: pointer;
	margin-top: 4px;
}

.collapse_pill:hover {
	background: var(--highlighted);
	color: var(--text);
}
```

- [ ] **Step 3: Commit**

```bash
git add static/style.css
git commit -m "feat: depth-colored collapse bars and reply pill styles"
```

---

## Chunk 4: Mobile Responsive Adaptations

### Task 10: Update mobile breakpoint styles

**Files:**
- Modify: `static/style.css` (mobile breakpoints)

- [ ] **Step 1: Read current mobile breakpoint rules**

Read `static/style.css` around lines 1934-2080 for the existing mobile breakpoints.

- [ ] **Step 2: Add/update mobile rules at 800px breakpoint**

Within the `@media screen and (max-width: 800px)` section, add:

```css
/* Hide body preview on mobile for all layouts */
.post_body.post_preview {
	display: none;
}

/* Ensure touch-friendly collapse bars */
.comment_left {
	min-height: 44px;
}
```

- [ ] **Step 3: Update 507px breakpoint for new flexbox layout**

In the `@media screen and (max-width: 507px)` section, remove the old grid-template-areas override (lines 2019-2027) since we no longer use grid. Replace with flexbox adjustments if needed:

```css
@media screen and (max-width: 507px) {
	.post {
		gap: 8px;
		padding: 8px;
	}

	.collapse_pill {
		width: 100%;
	}
}
```

- [ ] **Step 4: Build and verify**

Run: `cargo build 2>&1 | tail -5`
Expected: successful build

- [ ] **Step 5: Test at different viewport sizes**

Start the server and test at:
- Desktop (1200px+): Card should show body preview, full spacing
- Tablet (800px): Body preview hidden, sidebar below content
- Mobile (507px): Tighter spacing, full-width collapse pills

Run: `cargo run` and open in browser, use devtools responsive mode.

- [ ] **Step 6: Commit**

```bash
git add static/style.css
git commit -m "feat: mobile responsive adaptations for new feed layout"
```

---

### Task 11: Final integration test and cleanup

- [ ] **Step 1: Run full build**

Run: `cargo build 2>&1 | tail -10`
Expected: clean build with no warnings related to our changes

- [ ] **Step 2: Run existing tests**

Run: `cargo test 2>&1 | tail -20`
Expected: all existing tests pass

- [ ] **Step 3: Manual smoke test**

Start the server with `cargo run` and verify:
1. Visit `/r/pics` — posts show with large thumbnails
2. Switch layout in settings between card/clean/compact — density changes
3. Change "Posts per page" to 100, verify more posts load
4. Visit a post with deep comments — depth 2+ auto-collapsed
5. Click "N more replies" pill — expands inline
6. Click collapse bar — toggles comment open/closed
7. Test with different themes (dark, light, dracula) — no broken colors
8. Test on mobile viewport — body preview hidden, touch-friendly bars

- [ ] **Step 4: Clean up any dead CSS**

Remove any orphaned CSS rules that referenced the old grid layout (grid-template-areas for posts, old `.post_score` column styles, etc.).

- [ ] **Step 5: Final commit**

```bash
git add -A
git commit -m "feat: complete feed & comment UX redesign

Density-based layouts (card/clean/compact), depth-2 comment collapse,
50 posts per page default, responsive mobile adaptations."
```
