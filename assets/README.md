# HorizonDB brand assets

| File | Use |
| --- | --- |
| `logo.svg` | The mark: a dusk tile with the setting sun, the horizon and its reflection. Avatars, favicons, docs. Carries its own background, so it works on light and dark, and stays legible down to 16 px. |
| `logo.png` | 512×512 render of `logo.svg` for places that refuse SVG (GitLab project avatar, Discord, Bun/npm registries). |
| `banner.svg` | 1280×320 banner: mark, wordmark, tagline. Top of the README. Self-contained (mark inlined, text in outlines), because a README `<img>` cannot load other files or fonts. |
| `social.svg` / `social.png` | 1280×640 social preview (Open Graph, GitLab/GitHub social image, link embeds). |
| `icon-mono.svg` | Single-colour mark without the tile, drawn in `currentColor`. Monochrome contexts, masks, print, favicons on a known background. |

## The mark

HorizonDB's architecture, drawn as a sunset:

- **The sun, above the horizon**, is memory: every read and every write lands
  there and answers from there.
- **Its lower edge breaks into rows**, thicker as they near the horizon: dirty
  keys on their way down.
- **The horizon line** is the flush, the moment memory is written to disk
  (every `flush_interval_secs`, and on shutdown).
- **The reflection below** is PostgreSQL: the same rows, settled, quieter.
  Read as a stack, it is also the classic database symbol.

Tagline: **Memory above, PostgreSQL below.**

`logo.svg` uses `hz-` prefixed ids and wraps the drawing in `<g id="hz-mark">`
so it can be inlined into other SVGs (the banner and the social image do).

## Palette

| Name | Hex | Use |
| --- | --- | --- |
| Dawn Gold | `#FFE7A3` → `#FFB347` | Top of the sun, highlights. |
| Ember | `#FF7A45` | Glow, warm accents. |
| Horizon Rose | `#FF5A7A` | Primary accent: the sun's base, the "DB" end, links. |
| Dusk Indigo | `#11153A` → `#2A1A55` | Sky. Backgrounds, badge labels. |
| Deep Water | `#0D0F2A` → `#060818` | Below the horizon. Darkest surfaces. |
| Paper | `#FFF7EE` | Text on dark, the horizon line. |
| Haze | `#B9B3DA` | Secondary text on dark. |

The brand gradient runs top to bottom, Dawn Gold → `#FFB347` → Horizon Rose,
like the sun. In horizontal use (the "DB" of the wordmark, the reflection
rows) it runs `#FFC75F` → Horizon Rose, left to right.

## Type

The wordmark is set in [Fira Sans](https://github.com/mozilla/Fira):
"Horizon" in Bold with tight tracking, "DB" in ExtraBold with the brand
gradient. Taglines use Fira Sans Regular/Medium, technical labels Fira Mono
Medium in capitals. All three are under the SIL Open Font License 1.1. In the
SVGs the text is converted to outlines, so they render without the fonts.
The landing page (`site/`) loads the same families from Google Fonts.

## Rules

- Keep clear space around the mark of at least a quarter of its width.
- Write the name **HorizonDB**: one word, capital H, capital DB.
- Don't recolour the tile outside the palette, stretch it, rotate it, or put
  the sun above a light sky: the mark is always dusk.
- On busy or mid-tone backgrounds use `logo.svg` (with its tile), not
  `icon-mono.svg`.

## Re-rendering

`logo.png` and `social.png` are headless Chromium screenshots of the SVGs:

```bash
chromium --headless=new --hide-scrollbars --window-size=512,512 \
  --default-background-color=00000000 \
  --screenshot=assets/logo.png "file://$PWD/assets/logo.svg"
chromium --headless=new --hide-scrollbars --window-size=1280,640 \
  --screenshot=assets/social.png "file://$PWD/assets/social.svg"
```

After any change to the mark, re-inline it in `banner.svg` and `social.svg`
(copy the `<defs>` content and `<g id="hz-mark">`), and check it at 16, 32 and
48 px on both a light and a dark background.
