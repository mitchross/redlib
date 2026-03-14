# Redlib Feed & Comment UX Redesign

**Date:** 2026-03-13
**Status:** Approved

## Summary

A unified redesign of Redlib's feed layout, comment threading, and pagination to optimize for image/video-heavy subreddit browsing on mobile and desktop. The three existing layout modes (card, clean, compact) are reimagined as density levels of the same large-thumbnail-first design.

## Goals

1. Larger image previews — thumbnails are the primary way users decide what to tap
2. More content per page — reduce pagination friction
3. Collapse deep comment noise — surface top-level discussion
4. Responsive adaptation — same concepts tuned per viewport

## Non-Goals

- Infinite scroll or JavaScript-dependent features
- New layout mode names (keep card/clean/compact for backwards compatibility)
- Auto-detection of media vs text subreddits

---

## 1. Feed Layout: Density-Based Design

All three layouts share the same core structure: thumbnail alongside text in a flexbox row. They differ in spacing, thumbnail size, and metadata visibility.

### Card (Spacious)

- **Thumbnail**: 120×90px, right side, border-radius 5px
- **Title**: 14px, font-weight 500, line-height 1.4
- **Body preview**: Shown (1-2 line truncated preview of post body)
- **Metadata**: Subreddit (bold, accent color), timestamp, author
- **Footer**: Score (accent, bold), comment count, share link
- **Container**: Background `--post`, border `--panel-border`, border-radius 5px, padding 12px, margin-bottom 8px
- **~3 posts visible** on mobile viewport

### Clean (Normal)

- **Thumbnail**: 100×75px, right side, border-radius 4px
- **Title**: 13px, font-weight 500, line-height 1.3
- **Body preview**: Hidden
- **Metadata**: Subreddit (bold, accent color), timestamp only (no author)
- **Footer**: Score + comment count inline, compact
- **Container**: No background/border, separated by 1px border-bottom `#222`, padding 10px 0
- **~5 posts visible** on mobile viewport

### Compact (Dense)

- **Thumbnail**: 60×45px, left side, border-radius 3px
- **Title**: 12px, font-weight 500, single line with text-overflow ellipsis
- **Body preview**: Hidden
- **Metadata**: Subreddit, score, comment count, timestamp — all on one line, 10px font
- **Container**: No background/border, 1px border-bottom `#1a1a1a`, padding 6px 0, flex row with align-items center
- **8+ posts visible** on mobile viewport

### Template Changes

The `post_in_list` macro in `utils.html` needs restructuring:

- Remove the CSS grid layout (grid-template-areas) in favor of flexbox
- Thumbnail moves from a grid column to a flex child
- Score moves from a separate grid column into the footer row
- Body preview becomes conditional on layout mode
- Thumbnail sizing controlled by CSS variables per layout class

### CSS Changes

- New CSS variables: `--thumb-width`, `--thumb-height`, `--post-padding`, `--post-gap`
- Card: `--thumb-width: 120px; --thumb-height: 90px; --post-padding: 12px; --post-gap: 12px`
- Clean: `--thumb-width: 100px; --thumb-height: 75px; --post-padding: 10px 0; --post-gap: 10px`
- Compact: `--thumb-width: 60px; --thumb-height: 45px; --post-padding: 6px 0; --post-gap: 8px`

---

## 2. Comment Auto-Collapse

### Behavior

| Depth | Default State | Display |
|-------|--------------|---------|
| 0 | Open | Full comment |
| 1 | Open | Full comment |
| 2+ | Collapsed | "N more replies" pill |

### Implementation

The existing `<details>` element in `comment.html` already supports open/closed state. Changes needed:

- **Template (`comment.html`)**: Pass a `depth` counter through recursive comment rendering. When `depth >= 2`, omit the `open` attribute on `<details>`.
- **Collapse pill**: When collapsed, show a summary element styled as: background `#1a1a1a`, border-radius 4px, padding 8px 12px, with "▸ N more replies" text
- **Count calculation**: Count all descendant comments in the collapsed chain to show accurate "N more replies"

### Collapse Bar Styling

- Width: 3px (up from current 2px) for better touch targets
- Border-radius: 2px
- Color-coded by depth:
  - Depth 0: `var(--accent)` (red)
  - Depth 1: `#44aacc` (teal)
  - Depth 2+: `#888` (gray)
- Cursor: pointer
- Minimum touch target: 44px height (achieved via padding on the comment_left container)

### Settings

- New preference: "Comment collapse depth" — dropdown with options: 2 (default), 3, 5, 10, Off
- Cookie: `collapse_depth` with default `2`
- Env var: `REDLIB_DEFAULT_COLLAPSE_DEPTH`

---

## 3. Pagination

### Changes

- Pass `limit` parameter to Reddit API requests (currently omitted, defaults to 25)
- New preference: "Posts per page" — dropdown with options: 25, 50 (default), 100
- Cookie: `post_count` with default `50`
- Env var: `REDLIB_DEFAULT_POST_COUNT`

### Implementation

- In `src/subreddit.rs`: Add `limit` query parameter to the Reddit API URL, read from cookie/default
- In `templates/settings.html`: Add "Posts per page" dropdown in the content preferences section
- PREV/NEXT cursor-based pagination unchanged — just fetches more items per page

---

## 4. Responsive Adaptation

### Desktop (>800px)

- All feed layouts: same as described above
- Card mode: body preview shown
- Comment collapse bars: 2px width (mouse precision)
- Sidebar: right column as current

### Mobile (≤800px)

- Card mode: body preview hidden (thumbnail is enough context)
- Comment collapse bars: 3px width, 44px min touch target height
- "N more replies" pill: full-width for thumb reach
- Score: inline with footer (current 507px behavior applied at 800px)

### Breakpoints

Keep existing breakpoints (800px, 507px, 480px). Changes are additive CSS rules within those media queries.

---

## 5. Files to Modify

| File | Changes |
|------|---------|
| `templates/utils.html` | Restructure `post_in_list` macro: flexbox layout, conditional body preview, thumbnail sizing |
| `templates/comment.html` | Add depth counter, conditional `open` attribute, collapse pill summary, depth-colored bars |
| `templates/settings.html` | Add "Posts per page" and "Comment collapse depth" dropdowns |
| `static/style.css` | New density CSS variables, flexbox post layout, collapse bar styles, mobile adaptations |
| `src/subreddit.rs` | Read `post_count` cookie, pass `limit` param to API |
| `src/post.rs` | Read `collapse_depth` cookie, pass depth to comment template |
| `src/config.rs` | New env vars: `REDLIB_DEFAULT_POST_COUNT`, `REDLIB_DEFAULT_COLLAPSE_DEPTH` |
| `src/server.rs` | Register new cookie preferences |

---

## 6. Migration & Backwards Compatibility

- Existing `layout` cookie values (card/clean/compact) continue to work — same names, new styling
- New cookies (`post_count`, `collapse_depth`) default gracefully when absent
- All 18 themes continue to work — changes use existing CSS variables (`--accent`, `--post`, `--foreground`, etc.)
- No JavaScript added — all changes are server-side (Rust) and CSS
