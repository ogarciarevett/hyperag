# Hyperag brand

Hyperag is shared memory and retrieval for AI agents. The brand is one mark, one accent colour and one typeface family.

## The mark

A capital H on a 16 by 16 pixel grid. A small square is lifted out of the top of the right stem and sits above it in orange: the one item retrieval brings back. One grid unit is one pixel at 16 px, so the mark stays crisp at 16, 32 and 64 px.

| Part | Grid units |
| --- | --- |
| Stems | 4 wide, 11 tall |
| Counter | 4 wide |
| Crossbar | 3 tall, one unit above centre |
| Notch cut from the right stem | 2 by 2 |
| Orange tab | 3 by 3, one unit above the notch, flush with its left edge, one unit past the stem on the right |

Source geometry (`logo/mark-color.svg`):

```svg
<path d="M1 4h4v3h4V4h2v2h2v9H9v-5H5v5H1z" fill="#0A0A0A"/>
<path d="M11 0h3v3h-3z" fill="#FF5A1F"/>
```

## Files

| Folder | Contents |
| --- | --- |
| `logo/` | `mark-*` and `lockup-*` SVGs in four variants, plus `app-icon.svg` |
| `png/` | marks at 16, 32, 64, 128, 256 and 512 px, lockups at 1200 px, app icon at 180, 192, 512 and 1024 px |
| `favicon/` | `favicon.svg` (follows light and dark), `favicon.ico` (16, 32, 48, the mark on an ink tile drawn at 1x, 2x and 3x so every entry is pixel exact), `apple-touch-icon.png`, `icon-192.png`, `icon-512.png` |
| `readme/` | README hero, light and dark |
| `social/` | GitHub social preview, 1280 by 640 |
| `docs/` | docs header, 1600 by 400, light and dark |

Variants: `color` (ink and orange on light), `color-dark` (white and orange on dark), `mono-black` and `mono-white` (one colour, for print, engraving and places that allow no colour).

All text in the SVGs is converted to outlines, so the files render the same everywhere with no fonts installed.

## Palette

| Name | Hex | Use |
| --- | --- | --- |
| Ink | `#0A0A0A` | mark, wordmark and text on light; dark backgrounds |
| Paper | `#FFFFFF` | light backgrounds; mark on dark |
| Signal orange | `#FF5A1F` | the tab only, plus at most one highlight per view |
| Slate | `#5C6370` | secondary text on light |
| Mist | `#A1A7B3` | secondary text on dark |
| Line | `#E6E8EB` / `#1C1F24` | rules, grids and borders on light / dark |

Orange is an accent, never a background or a body text colour. On white it is for shapes and large text only; it is too light for small text.

## Typography

- Wordmark: Inter ExtraBold, lowercase, tracking -2 percent. Use the outlined SVG, never retype it.
- Headings and body: Inter (Regular 400, Medium 500, Bold 700).
- Code: JetBrains Mono.

Both families are free under the SIL Open Font License and available from Google Fonts.

## Clear space and size

- Keep clear space of one stem width (4 grid units, a quarter of the mark height) on every side of the mark and the lockup.
- In the lockup the gap from the right stem to the ink of the h is exactly one stem width (4 units), and the stem height matches the height of the h in the wordmark.
- Smallest sizes: mark 16 px, lockup 96 px wide.

## Do not

- Do not redraw, round, outline or slant the mark, and do not move the tab.
- Do not colour the H orange or the tab any colour other than orange or the mono colour.
- Do not add gradients, shadows, glows or textures.
- Do not stretch, rotate or crop the mark.
- Do not place the colour mark on a mid-tone or busy image; use the mono variants there.
- Do not set the wordmark in another typeface or in capitals.
- Do not use the mark at non-integer multiples of 16 px in pixel contexts (favicons, toolbars), where it blurs.

## GitHub social preview

`social/social-preview.png` is ready. Set it in the repository settings (Settings, General, Social preview) when the repository goes public.
