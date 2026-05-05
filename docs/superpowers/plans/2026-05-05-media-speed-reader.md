# Media-Speed Reader Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [x]`) syntax for tracking.

**Goal:** Build a calmer Reader-style post/comment experience and an optional gallery layout for subreddit media browsing.

**Architecture:** Reuse the existing server-rendered Askama templates and the existing `layout` preference. Gallery mode is a subreddit-only wrapper class around the current post list markup; non-subreddit listing pages keep normal list behavior even if the body has `gallery`.

**Tech Stack:** Rust, Askama templates, CSS, native `<details>` comment collapse, Cargo tests/build.

---

### Task 1: Preference Surface

**Files:**
- Modify: `templates/settings.html`
- Test: `src/utils.rs`

- [x] **Step 1: Write the failing test**

Add a unit test in `src/utils.rs` proving `gallery` remains a serializable layout value:

```rust
#[test]
fn serialize_gallery_layout_pref() {
	let prefs = Preferences {
		layout: "gallery".to_owned(),
		..Default::default()
	};
	let urlencoded = serde_urlencoded::to_string(prefs).expect("Failed to serialize Prefs");
	assert!(urlencoded.contains("layout=gallery"));
}
```

- [x] **Step 2: Run test to verify it fails or exposes current behavior**

Run: `cargo test serialize_gallery_layout_pref`

- [x] **Step 3: Add gallery to the settings dropdown**

Change the layout options in `templates/settings.html` from:

```html
{% call utils::options(prefs.layout, ["card", "clean", "compact"], "card") %}
```

to:

```html
{% call utils::options(prefs.layout, ["card", "clean", "compact", "gallery"], "card") %}
```

- [x] **Step 4: Run test again**

Run: `cargo test serialize_gallery_layout_pref`

### Task 2: Subreddit Gallery Wrapper

**Files:**
- Modify: `templates/subreddit.html`

- [x] **Step 1: Add gallery wrapper class**

Change the posts container to:

```html
<div id="posts" {% if prefs.layout == "gallery" %}class="post-gallery"{% endif %}>
```

- [x] **Step 2: Compile templates**

Run: `cargo check`

### Task 3: Gallery and Reader CSS

**Files:**
- Modify: `static/style.css`

- [x] **Step 1: Add gallery CSS**

Add CSS for `#posts.post-gallery` that uses responsive grid columns, puts `.post_thumbnail` above `.post_content`, hides body previews, clips titles, and keeps score/comment metadata compact.

- [x] **Step 2: Soften post detail CSS**

Reduce score panel emphasis, title weight, metadata opacity, and footer weight for `.post.highlighted`.

- [x] **Step 3: Soften comment CSS**

Make `.comment_score`, `.line`, `.comment_data`, `.comment_body`, `.collapse_pill`, and mobile comment spacing quieter and more reader-like.

- [x] **Step 4: Verify CSS selectors do not affect non-gallery listing pages**

Run: `rg -n "post-gallery|body.gallery|comment_score|collapse_pill" static/style.css templates`

### Task 4: Verification and Release Prep

**Files:**
- All changed files

- [x] **Step 1: Run focused tests**

Run: `cargo test serialize_gallery_layout_pref`

- [x] **Step 2: Run project tests/build**

Run: `cargo test`

Run: `cargo build --release`

- [x] **Step 3: Inspect diff**

Run: `git diff --stat` and `git diff --check`

- [x] **Step 4: Capture Mink note**

Run: `mink note --project redlib --category projects` with a concise summary of the gallery/Reader design decision and verification commands.

- [x] **Step 5: Commit, tag, push, and update Talos repo**

After verification, commit the redlib changes, create the requested tag, push the branch/tag, then update the Talos repository reference/image according to the local Talos repo layout.
