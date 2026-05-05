# Redlib Media-Speed Reader Design

**Date:** 2026-05-05
**Status:** Implemented

## Summary

Refine Redlib's feed, post detail, and comment reading experience for fast browsing of image- and video-heavy subreddits. The design keeps media discovery quick, calms down the visual weight of post and comment chrome, and prevents large reply chains from burying other top-level comments.

## Goals

1. Make image/video browsing feel fast and scannable.
2. Make post detail pages quieter and less bold without losing hierarchy.
3. Keep top-level comments easy to scan by collapsing reply chains after one visible reply.
4. Add an optional gallery browsing mode for media-heavy subreddit listings while preserving list view.
5. Improve comment controls, search, and sort ergonomics without adding JavaScript.
6. Preserve existing themes, layout modes, and current server-rendered behavior.

## Non-Goals

- Infinite scroll.
- JavaScript-driven comment loading or filtering.
- Reworking Reddit API fetch behavior beyond existing preference-driven limits.
- A new theme or brand direction.
- Heavy markup refactors outside the post/thread/comment surfaces.
- Replacing the current feed/list experience.

## Design Direction

Use a **Media-Speed Reader** direction:

- Feed pages stay thumbnail-forward and dense enough for fast browsing.
- Subreddit listings can opt into a gallery grid for image/video-heavy browsing.
- Post detail pages use a flatter Reader treatment: less score emphasis, softer metadata, and fewer heavy accent blocks.
- Comment threads prioritize top-level scanning. A top-level comment and one direct reply remain visible; deeper replies collapse behind a compact reply-count pill.
- Controls are grouped and visually quiet, so sort/search remain available without dominating the page.

## Feed Experience

Feed cards should continue using the existing large-thumbnail layout from the earlier feed redesign. The changes here are polish-oriented:

- Keep thumbnails and video/image previews visually dominant in feed rows.
- Reduce overly heavy score emphasis where it competes with titles and thumbnails.
- Maintain existing `card`, `clean`, and `compact` layout modes.
- Avoid adding inline full media to list rows; the full media experience remains on post detail pages.

## Optional Gallery Mode

Add an optional gallery mode for subreddit-style listings. It should sit alongside list view rather than replace it.

Behavior:

- Users can choose `gallery` as a listing layout mode.
- Gallery cards render in a responsive grid with media thumbnail first, followed by title and compact metadata.
- Cards include enough text to identify the post: title, score/comment count, and media/type cues where available.
- Text/self posts are represented with a quiet placeholder treatment so image/video posts remain the main value of the mode.
- The existing list modes remain available for users who prefer dense text scanning.

Scope:

- Start with subreddit listings, where media browsing is the clearest value.
- Reuse existing post data and thumbnail/media fields.
- Avoid full media embeds inside the grid; open the post detail page for full image/video/gallery viewing.
- Keep gallery responsive: multi-column on desktop/tablet and two columns on phones, with stable tile dimensions and clipped text.

## Post Detail Experience

The highlighted post should become calmer:

- Reduce bold score treatment and accent panel strength.
- Keep title readability high, but reduce negative letter spacing and avoid oversized weight.
- Make metadata more compact and secondary.
- Keep media content prominent and avoid adding decorative containers around images/videos.
- Preserve current responsive grid behavior unless a small CSS adjustment is required for readability.

## Comment Behavior

Default comment collapse behavior:

- Depth 0: top-level comments open.
- Depth 1: direct replies open.
- Depth 2 and deeper: collapsed by default.

This matches `collapse_depth = 2`: users can see one reply for context, then the rest of a chain becomes a compact "N more replies" row. This prevents a single high-activity joke or argument chain from consuming the page and hiding other top-level comments.

Highlighted comments should still open, and filtered or stickied moderator comments should keep their existing collapse behavior.

## Comment Visual Design

Use a Reader-style thread treatment:

- Make scores secondary: smaller, lighter, and less boxy.
- Use subtle thread lines instead of bright multi-color rails.
- Keep author and timestamp metadata compact and readable.
- Make collapsed reply pills quiet but easy to hit.
- Use spacing that separates top-level comments without making every comment feel like a card.
- On narrow screens, preserve overflow prevention for deep nesting and keep tap targets reasonable.

## Comment Controls

The comment sort/search row should be clearer and calmer:

- Keep comment count, sort select, apply button, and comment search in one responsive control area.
- Use consistent heights, borders, and focus states.
- Stack controls cleanly on small screens.
- Make active search state readable with an "All comments" escape link.

## Accessibility and Interaction

- Continue using native `<details>` and `<summary>` for collapsible comments.
- Preserve keyboard and screen-reader-accessible controls.
- Add or retain visible focus states for sort/search/comment summary controls.
- Avoid hiding meaningful text behind icons.

## Implementation Scope

Primary files:

- `static/style.css`: Reader polish for post detail, comments, collapse pills, and comment controls.
- `templates/subreddit.html` and `templates/utils.html`: Add optional gallery rendering for subreddit listings while preserving existing list rendering.
- `templates/settings.html`: Add `gallery` to the existing layout preference. On non-subreddit listing pages, `gallery` falls back to the normal card/list rendering until those surfaces are explicitly supported.
- `templates/post.html`: Minor class or structure updates only if needed for better control layout.
- `templates/comment.html`: Minor label/copy/class adjustments only if needed; keep existing depth and descendant-count behavior.

No Rust changes are required for comment collapse because depth, descendant counts, auto-collapse, and the `collapse_depth` preference already exist. Gallery reuses the existing `layout` preference, so it should not require a new cookie, config key, or settings field.

## Testing

- Build or run template compilation checks through the project test/build command.
- Verify `collapse_depth = 2` keeps top-level comments and one reply open while collapsing deeper replies.
- Verify gallery mode preserves links, thumbnails, titles, score/comment metadata, NSFW/spoiler blur, and pagination.
- Check desktop and mobile layouts for post detail pages, image/video posts, and deep comment nesting.
- Check at least one light and one dark theme because the CSS relies on theme variables.
