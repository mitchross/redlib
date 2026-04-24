## Design Context

### Users
A diverse audience spanning privacy-conscious readers who want Reddit content without tracking, power users and developers who self-host their own instances, and casual visitors arriving via shared links. The common thread: they all want fast, distraction-free access to content — especially image and video heavy subreddits on mobile.

### Brand Personality
**Rebellious, Open, Honest.** Redlib exists as an act of defiance against corporate surveillance and engagement-maximizing design. It wears its values openly — no tricks, no dark patterns, no hidden agendas. The interface should feel like it respects the user's intelligence and time.

### Aesthetic Direction
- **Visual tone**: Distinctive with personality — not bland minimalism, but purposeful design that feels alive
- **References**: The user wants a mobile-first, image-forward browsing experience. Think gallery-style layouts for image-heavy subs, large previews instead of tiny thumbnails, fast navigation between posts
- **Anti-references**: Avoid new Reddit's bloat, generic Bootstrap/Material sameness, and lifeless ultra-minimalism. Also avoid over-designed flashiness — the personality should come from smart UX choices, not visual noise
- **Theme**: Dark mode default with red accent (#d54455). 18 theme options already available.

### Accessibility
Best-effort approach — no formal WCAG compliance target, but follow good practices: visible focus indicators, sufficient contrast for readability, semantic HTML, and alt text for meaningful images. All animations respect `prefers-reduced-motion: reduce`.

### Design Principles

1. **Images and video first**: Optimize for media-heavy subreddits. Large previews, not tiny thumbnails. Make visual content the hero of the layout, especially on mobile.
2. **More content, less pagination**: Default to showing more content per page. Reduce friction from "next page" clicks. The user should be able to scroll and consume, not constantly click through.
3. **Smart information density**: Collapse noise (joke chains, repetitive comments) while keeping signal. Give users tools to quickly find the substance in threads.
4. **Fast and tactile navigation**: Mobile experience should feel snappy and fluid. Quick transitions, easy back/forward, minimal loading friction.
5. **Personality through craft**: The interface should evoke delight — not through flashy animations, but through thoughtful details, good typography, and interactions that feel right.
