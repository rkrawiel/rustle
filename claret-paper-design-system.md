# Claret & Paper — Design System Spec

> **For the AI reading this:** this file is the single source of truth for the visual style of any app, site or component you build in this conversation. Use these tokens and rules exactly. Do not introduce new colours, webfonts, gradients, shadows or rounded "SaaS" styling. If something is not covered here, extend it in the same spirit (quiet, editorial, warm, high-contrast) and say what you added.

Owner: Robert Krawiel · Version 1.0 · 2026-10-02
Reference implementation: Astro 7 personal site (`src/styles/global.css`).

---

## 1. Character

Editorial, senior, calm. Think a well-set printed report, not a startup dashboard.

- Warm off-white **paper** background, near-black warm **ink**, one deep **claret** (oxblood) accent.
- Colour is used sparingly and on purpose: roughly 90% neutrals, accent only for links, current state, focus, and one deliberate highlight per screen.
- Serif headings, sans-serif body. Generous whitespace, thin 1px rules instead of boxes and shadows.
- Explicitly avoid: Azure/corporate blue, SaaS purple/violet, neon-on-black dark mode, gradients, glassmorphism, drop shadows, large border radii, emoji as UI.

## 2. Colour tokens

Tokens are named by **role**, never by hue. Light and dark switch automatically with the OS via `light-dark()`.

| Token | Light | Dark | Role |
|---|---|---|---|
| `--bg` | `#FAF8F5` | `#161413` | Page background (paper / warm charcoal) |
| `--surface` | `#F1EDE7` | `#211E1C` | Cards, code blocks, inline code, tags |
| `--text` | `#1C1917` | `#EDE8E3` | Primary text, headings |
| `--text-2` | `#57534E` | `#B8B2AC` | Secondary text, metadata, inactive nav |
| `--accent` | `#8A1C2B` | `#EE9AA3` | Links, current state, focus ring, highlight rule |
| `--accent-2` | `#6E1422` | `#F6BCC2` | Accent hover / active |
| `--border` | `#DDD6CE` | `#3A3532` | Hairline dividers (decorative only) |
| `--success` | `#2E6B3F` | `#6FBF82` | Success status |
| `--warning` | `#8A5300` | `#E0A84A` | Warning status |
| `--error` | `#B42318` | `#F07A6A` | Error status |

Text on accent fill (e.g. the monogram, `::selection`) uses `--bg` as the foreground.

### Measured contrast (WCAG 2.x, against `--bg` / `--surface`)

| Token | Light | Dark |
|---|---|---|
| text | 16.5 / 15.0 | 15.1 / 13.6 |
| text-2 | 7.2 / 6.5 | 8.8 / 7.9 |
| accent | 8.7 / 7.9 | 8.6 / 7.7 |
| accent-2 | 11.1 / 10.1 | 11.3 / 10.2 |
| success | 6.0 / 5.5 | 8.3 / 7.5 |
| warning | 6.0 / 5.4 | 8.6 / 7.8 |
| error | 6.2 / 5.6 | 6.7 / 6.1 |
| `--bg` on accent | 8.7 | 8.6 |
| border | 1.4 | 1.5 |

Everything text-bearing passes AA; most passes AAA. `--border` is **below 3:1 by design**: use it only for decorative separators. Form controls and any boundary that carries meaning must use `--text-2` for the border.

## 3. Typography

System font stacks only. **No webfonts** (first paint is the final design, zero font bytes).

```css
--font-serif: "Iowan Old Style", "Palatino Linotype", Palatino, "URW Palladio L", P052, Georgia, serif;
--font-sans:  system-ui, -apple-system, "Segoe UI", Roboto, "Helvetica Neue", sans-serif;
--font-mono:  ui-monospace, "Cascadia Code", "SF Mono", Menlo, Consolas, monospace;
```

- Headings (h1–h3), brand name, blockquotes, large numerals: **serif**, weight 600, line-height 1.15, letter-spacing −0.01em (h1 hero −0.02em), `text-wrap: balance`.
- Body and UI: **sans**, weight 400, 17px, line-height 1.6 (long-form prose 1.7), `text-wrap: pretty`.
- Dates and numbers in lists: `font-variant-numeric: tabular-nums`.

### Type scale (major third, ratio 1.25)

```css
--step--1: 0.85rem;    /* metadata, footer, tags */
--step-0:  1.0625rem;  /* 17px body */
--step-1:  1.33rem;    /* h3, lede, dt */
--step-2:  1.66rem;    /* h2 */
--step-3:  2.07rem;    /* article h1 */
--step-4:  clamp(2.3rem, 1.6rem + 3vw, 3.25rem); /* page / hero h1 */
```

## 4. Layout & spacing

```css
--measure: 68ch;                          /* max line length for reading */
--wide:    72rem;                         /* page container */
--gutter:  clamp(1.25rem, 4vw, 2.5rem);   /* side padding */
--space:   1.5rem;
--radius:  3px;                           /* the only radius; never pill shapes */
```

- Main vertical padding: `clamp(2.5rem, 7vw, 5rem)`. Section gaps: `clamp(3.5rem, 9vw, 6rem)`.
- Long-form flow: `.flow > * + * { margin-block-start: var(--flow, 1em) }`; prose `--flow: 1.1em`, h2 top 2.2em, h3 top 1.8em, pre/blockquote 1.6em.
- Lists of items (posts, roles): two-column grid `9.5rem 1fr` (date column + content), 1px `--border` rule between rows, collapses to one column under 40rem.
- Separate with hairlines and whitespace, not cards. If a card is unavoidable: `--surface` fill, 1px `--border`, `--radius`, no shadow.

## 5. Component patterns

**Links** — always underlined in body text (claret alone fails for protan users):
```css
a { color: var(--accent); text-decoration-thickness: 1px; text-underline-offset: 0.18em; }
a:hover { color: var(--accent-2); text-decoration-thickness: 2px; }
```
Title links in lists may be `--text` with no underline at rest, accent + underline on hover.

**Navigation** — inactive items `--text-2`, no underline. Current page = `--text` + weight 600 + 2px `--accent` bottom border. Never indicate state by colour alone.

**Focus** — `outline: 2px solid var(--accent); outline-offset: 3px;` on `:focus-visible`. Never remove it.

**Header** — brand left (monogram + serif name), nav right, 1px `--border` bottom rule. Monogram: 2rem square, `--accent` fill, `--bg` text, serif 700, `--radius`.

**Hero accent** — the one deliberate colour moment per page: a short rule under the h1 (4.5rem × 3px, `--accent`, 1.75rem above).

**Buttons** (extension for apps) — primary: `--accent` fill, `--bg` text, `--radius`, hover `--accent-2`. Secondary: transparent, 1px `--text-2` border, `--text` label. No shadows, no gradients, no pills.

**Code** — `--surface` background, 1px `--border`, `--radius`, mono at 0.9em, line-height 1.5, horizontal scroll. Inline code: `--surface`, padding `0.1em 0.35em`. Syntax highlighting: github-light / github-dark, switched with the theme.

**Blockquote** — 3px `--accent` inline-start border, 1.25rem padding, serif at `--step-1`, line-height 1.4.

**Tags / chips** — `--surface` fill, `--text-2` text, `--step--1`, `--radius`, padding `0.15rem 0.6rem`.

**List markers** — `li::marker { color: var(--accent) }` for outcome/bullet lists.

**Status messages** — text or left border in `--success` / `--warning` / `--error` **plus** an icon or label word ("Error:"). Background stays `--surface`; never fill with saturated colour.

**Selection** — `background: var(--accent); color: var(--bg);`

**Images / portrait** — rectangular, 4:5, thin claret outline with offset (no circular crops). AVIF/WebP, explicit width/height.

**Timeline motif** (optional) — dashed `--border` line for future/unreached, solid `--accent` line for travelled progress, large serif year numerals, nodes that fill with accent. CSS scroll-driven animation, static fallback.

## 6. Accessibility rules (non-negotiable)

- WCAG 2.2 AA minimum; aim AAA for body text. Check APCA for small/thin text.
- Never convey meaning by colour alone (underline, weight, icon or text label as well).
- Visible `:focus-visible` everywhere; skip link to `#main`.
- Respect `prefers-reduced-motion: reduce` (disable animations and transitions).
- Print stylesheet forces light scheme, hides nav/footer, links inherit colour.

## 7. Performance & engineering constraints

- Zero client JavaScript by default. Interactivity via CSS first; JS (Svelte 5 islands) only when state is genuinely required.
- Zero webfonts, no icon fonts (inline SVG only, coloured with `currentColor` or tokens).
- Target: Lighthouse 100, 15–30 KB per page excluding images; inline critical CSS.
- Theme follows the OS (`color-scheme: light dark`); no manual toggle unless asked.
- `light-dark()` requires 2024+ browsers. If older Safari matters, replace with a `@media (prefers-color-scheme: dark)` block redefining the same tokens.
- Animations: use longhand `animation-*` properties next to `animation-timeline` (minifiers can merge shorthand into invalid CSS).

## 8. Drop-in CSS

```css
:root {
  color-scheme: light dark;

  /*                     light      dark     */
  --bg:        light-dark(#FAF8F5, #161413);
  --surface:   light-dark(#F1EDE7, #211E1C);
  --text:      light-dark(#1C1917, #EDE8E3);
  --text-2:    light-dark(#57534E, #B8B2AC);
  --accent:    light-dark(#8A1C2B, #EE9AA3);
  --accent-2:  light-dark(#6E1422, #F6BCC2);
  --border:    light-dark(#DDD6CE, #3A3532);
  --success:   light-dark(#2E6B3F, #6FBF82);
  --warning:   light-dark(#8A5300, #E0A84A);
  --error:     light-dark(#B42318, #F07A6A);

  --font-serif: "Iowan Old Style", "Palatino Linotype", Palatino, "URW Palladio L", P052, Georgia, serif;
  --font-sans: system-ui, -apple-system, "Segoe UI", Roboto, "Helvetica Neue", sans-serif;
  --font-mono: ui-monospace, "Cascadia Code", "SF Mono", Menlo, Consolas, monospace;

  --step--1: 0.85rem;
  --step-0:  1.0625rem;
  --step-1:  1.33rem;
  --step-2:  1.66rem;
  --step-3:  2.07rem;
  --step-4:  clamp(2.3rem, 1.6rem + 3vw, 3.25rem);

  --measure: 68ch;
  --wide: 72rem;
  --gutter: clamp(1.25rem, 4vw, 2.5rem);
  --space: 1.5rem;
  --radius: 3px;
}

body {
  background: var(--bg);
  color: var(--text);
  font: 400 var(--step-0) / 1.6 var(--font-sans);
  -webkit-font-smoothing: antialiased;
}
h1, h2, h3 {
  font-family: var(--font-serif);
  font-weight: 600;
  line-height: 1.15;
  letter-spacing: -0.01em;
  text-wrap: balance;
}
a { color: var(--accent); text-decoration-thickness: 1px; text-underline-offset: 0.18em; }
a:hover { color: var(--accent-2); text-decoration-thickness: 2px; }
:focus-visible { outline: 2px solid var(--accent); outline-offset: 3px; }
::selection { background: var(--accent); color: var(--bg); }
hr { border: 0; border-top: 1px solid var(--border); }

@media (prefers-reduced-motion: reduce) {
  *, *::before, *::after { transition: none !important; animation: none !important; }
}
```

### Tailwind v4 mapping (if Tailwind is used)

```css
@import "tailwindcss";
@theme inline {
  --color-bg: var(--bg);
  --color-surface: var(--surface);
  --color-text: var(--text);
  --color-text-2: var(--text-2);
  --color-accent: var(--accent);
  --color-accent-2: var(--accent-2);
  --color-border: var(--border);
  --color-success: var(--success);
  --color-warning: var(--warning);
  --color-error: var(--error);
  --font-serif: var(--font-serif);
  --font-sans: var(--font-sans);
  --font-mono: var(--font-mono);
  --radius-sm: 3px;
}
```
Keep the `:root` token block above as the source; Tailwind only references it. Do not use Tailwind's default palette classes (`blue-500`, `slate-*`, etc.).

### Non-web targets

For native/desktop apps, charts or slides, use the same hex values: light set as default, dark set when the platform is in dark mode. Chart series order: `--accent`, `--text-2`, `--success`, `--warning`, then tints of accent; gridlines in `--border`.

## 9. Checklist before delivering

- [ ] Only the tokens above; no hard-coded colours outside the token block.
- [ ] Serif headings, system sans body, no webfont requests.
- [ ] Accent used for links/state/focus plus at most one highlight per screen.
- [ ] Body links underlined; state never shown by colour alone.
- [ ] Works in light and dark, at 390px width, and with reduced motion.
- [ ] No shadows, gradients, pills, or blue/purple.
