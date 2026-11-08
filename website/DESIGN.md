---
name: rekcod website
description: Minimal typography and Markdown documentation on a white surface.
colors:
  background: "#fff"
  foreground: "#20201e"
  muted: "#62625d"
  line: "#e7e7e3"
  surface: "#f7f7f5"
  hover: "#efefec"
  focus: "#3065c7"
  button-hover: "#3f3f39"
  selection: "#dce6f8"
  logo-ink: "#000"
typography:
  display:
    fontFamily: "'Geist Variable', sans-serif"
    fontSize: "clamp(64px, 8vw, 88px)"
    fontWeight: 550
    lineHeight: 1.05
    letterSpacing: "-0.04em"
  headline:
    fontFamily: "'Geist Variable', sans-serif"
    fontSize: "36px"
    fontWeight: 550
    lineHeight: 1.2
    letterSpacing: "-0.03em"
  title:
    fontFamily: "'Geist Variable', sans-serif"
    fontSize: "23px"
    fontWeight: 550
    lineHeight: 1.3
    letterSpacing: "-0.025em"
  subheading:
    fontFamily: "'Geist Variable', sans-serif"
    fontSize: "18px"
    fontWeight: 550
    lineHeight: 1.4
    letterSpacing: "-0.015em"
  lead:
    fontFamily: "'Geist Variable', sans-serif"
    fontSize: "24px"
    fontWeight: 400
    lineHeight: 1.55
    letterSpacing: "-0.025em"
  body:
    fontFamily: "'Geist Variable', sans-serif"
    fontSize: "16px"
    fontWeight: 400
    lineHeight: 1.75
  label:
    fontFamily: "'Geist Variable', sans-serif"
    fontSize: "14px"
    fontWeight: 500
    lineHeight: 1.6
  code:
    fontFamily: "'Geist Mono Variable', monospace"
    fontSize: "13px"
    fontWeight: 400
    lineHeight: 1.8
rounded:
  small: "4px"
  control: "6px"
  panel: "8px"
spacing:
  small: "8px"
  compact: "12px"
  body: "16px"
  gutter: "24px"
  group: "32px"
  column: "40px"
  section: "48px"
components:
  button-primary:
    backgroundColor: "{colors.foreground}"
    textColor: "{colors.background}"
    typography: "{typography.label}"
    rounded: "{rounded.control}"
    padding: "10px 16px"
  button-primary-hover:
    backgroundColor: "{colors.button-hover}"
  text-link:
    textColor: "{colors.foreground}"
  search-field:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.muted}"
    rounded: "{rounded.control}"
    padding: "0 12px"
  document-select:
    backgroundColor: "{colors.background}"
    textColor: "{colors.foreground}"
    rounded: "{rounded.small}"
    padding: "8px"
  nav-link:
    textColor: "{colors.muted}"
    rounded: "{rounded.small}"
    padding: "8px 10px"
  nav-link-current:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.foreground}"
  code-block:
    backgroundColor: "{colors.surface}"
    typography: "{typography.code}"
    rounded: "{rounded.panel}"
    padding: "24px 72px 24px 24px"
  copy-code:
    backgroundColor: "{colors.surface}"
    textColor: "{colors.muted}"
    rounded: "{rounded.small}"
    padding: "8px"
  search-results:
    backgroundColor: "{colors.background}"
    rounded: "{rounded.panel}"
    padding: "8px"
  wordmark:
    textColor: "{colors.foreground}"
---

# Design System: rekcod website

## Overview

**Creative North Star: "Super minimal Markdown reader"**

The user's explicit direction is super minimal. The landing page and documentation
share an untextured white surface, near-black text and fine gray dividers. The
minimal brief overrides ornamental concepts. Direction reference: `seed:86aa5abe0dc81372`.

FORM: a code-led Markdown reader with a typographic landing page. Self-hosted Geist
carries the reading text; Geist Mono carries code. The README introduction supplies
the landing lead. Spacing, type size and full-surface background changes establish
hierarchy. No decorative imagery, shadows, animation or transitions are implemented.

The user explicitly chose the Docker whale rotated 180 degrees, in black and white,
as the logo. The same SVG identifies the header and the browser favicon. This brand
asset is part of the minimal interface.

**Key Characteristics:**

- White page, near-black text and quiet neutral surfaces.
- Geist text with Geist Mono code.
- A centered landing column and responsive documentation grid.
- Native document selection on mobile and visible keyboard focus.
- A black and white Docker whale rotated 180 degrees.

Source of truth: [global stylesheet](src/styles/global.css), [shared layout](src/layouts/Base.astro),
[landing page](src/pages/index.astro), [document reader](src/pages/docs/[...slug].astro)
and [search](src/components/Search.astro). The frontmatter records implemented values;
the sidecar supplies component previews and metadata.

## Colors

### Primary

Foreground is the primary action fill as well as the main text color. Button hover
is a slightly lighter dark neutral. Focus blue identifies keyboard position and the
input caret; it is not an ornamental accent. Selection is the text-selection fill.

### Neutral

Background is the page canvas. Muted is used for secondary prose and navigation.
Line separates sections, table rows and outlined containers. Surface groups code,
fields and current navigation; hover strengthens the code-copy control state.

The seven root colors are defined in [global.css](src/styles/global.css#L13).
Button hover and selection are defined by their component and global state rules.
Logo ink belongs to the monochrome identity asset in [logo.svg](public/logo.svg).
Shiki supplies syntax token colors separately through `github-light-high-contrast`
in [content.ts](src/lib/content.ts#L141). Keep token contrast readable against surface.
The sidecar's synthesized tonal ramps are swatch previews, not additional site colors.

**The Full Surface Rule.** Use a uniform background for selection and grouping. Borders divide or outline; they never form decorative left-edge rails.

## Typography

Both variable font families are imported in [Base.astro](src/layouts/Base.astro#L2)
and assigned by the root font tokens. The hierarchy uses individually chosen sizes;
it does not follow a fixed mathematical scale.

| Role | Use |
| --- | --- |
| Display | Landing title; fluid size from the display token. |
| Headline | Document h1; reduced on mobile. |
| Title and subheading | Markdown h2 and h3; more space above than below. |
| Lead and body | README introduction and Markdown reading text. |
| Label and code | Primary action labels and code blocks. |

Markdown h4 through h6 retain body size with a semibold weight (600). Header links
use regular text (14px); sidebar links use compact text (13px); on-page contents use
small text (12px). Inline code is relative to its surrounding text (0.84em). Tables
use compact text (13px), a tighter line height (1.6) and tabular numerals.
Base interface text uses line height (1.6); Markdown uses the body token.
These rules live in [global.css](src/styles/global.css#L72).

**The Reading Rule.** Keep prose at the body size and line height. Reserve the display treatment for the landing title and use heading spacing to expose document structure.

## Layout

The landing column is centered with a bounded width (`min(100% - 48px, 736px)`).
The desktop hero has generous top and bottom padding (80px and 64px). The README
starts after a divider and inset (44px). Paragraphs use vertical margins (16px);
main Markdown sections use a larger top margin (48px). The spacing tokens capture
repeated intervals, not an exhaustive scale: compact text also uses smaller optical gaps.

The desktop documentation shell is centered (1200px maximum) with outer padding
(40px), a left navigation column (224px), a flexible main column and an on-page
contents column (168px), separated by gaps (40px). Both side columns are sticky
(24px from the viewport top), independently scrollable and bounded by viewport
height. The shared header follows the same outer width. See [layout rules](src/styles/global.css#L44).

| Viewport rule | Implemented change |
| --- | --- |
| At least 1440px | Shell grows to 1320px; gaps become 48px; contents column becomes 184px; main document is capped at 72ch. |
| At most 1230px | Footer receives 40px side margins. |
| At most 1100px | Contents column hides; sidebar becomes 208px; two-column reader remains. |
| At most 760px | Sidebar hides; native document selector appears; search occupies a full second header row; reader becomes one column with 24px gutters. |

Mobile heading sizes become smaller (h1 32px, h2 22px, lead 21px); h2 top spacing
becomes tighter (40px). The hero uses equal vertical padding (48px). Code blocks
use smaller text (12px), less left padding (16px) and space for the copy button
(64px on the right). Table cell padding reduces to 10px vertically and 12px
horizontally. All breakpoint rules are in [global.css](src/styles/global.css#L148).

**The Contained Overflow Rule.** Code blocks and tables scroll within their containers. Preserve native browser history gestures and document scrolling.

## Elevation & Depth

The interface is flat. No box shadows, gradients or backdrop filters are present.
Neutral fills and thin outlines separate containers. Search results sit above
content by positioning and stacking order, with the same flat outline as other
panels. Focus is an outline rather than a shadow. State changes are immediate;
no animation or transition tokens are defined. Reduced-motion preferences retain
automatic scrolling in [global.css](src/styles/global.css#L190).

## Shapes

Small corners belong to inline code, navigation rows, copy controls and the document
selector. Control corners belong to the main action and search field. Panel corners
belong to code blocks, search results, blockquotes and disclosure containers. Borders
are fine uniform strokes (1px). The search shortcut has a smaller corner (3px).
Arrow and search icons are inline SVG strokes (16px), with no icon font or glyph icon.

**The Logo Rule.** Use the Docker whale rotated 180 degrees in black and white for the header and favicon. The app bar shows only the symbol. Keep its geometry intact.

## Components

### Brand link

The home link shows the inverted Docker whale (32px square) without visible text.
The symbol is centered in a target at least 44px square. Its accessible label is
`rekcod home`. The header and favicon reuse [logo.svg](public/logo.svg).
Sources: [shared header](src/layouts/Base.astro#L33), [wordmark styles](src/styles/global.css#L46).

### Actions

The primary link is a dark filled control with a compact label, an inline arrow and
a minimum height (44px). Its hover changes only the fill. The secondary action is
an unfilled text link with the same minimum height; hover adds an underline.
Code-copy controls use the surface fill, a small label (11px) and minimum dimensions
(44px square). Copy success changes the label to `Copied` for two seconds; clipboard
failure selects the code and changes the label to `Select code`.
Sources: [action styles](src/styles/global.css#L108), [copy behavior](src/layouts/Base.astro#L48).

### Search and document selection

Search uses a fine border, surface fill, search SVG and an input (13px). The parent
field owns the focus outline (2px, offset 2px). The result panel opens below the
field with a gap (8px), at least the field width and a desktop minimum width
(360px). Its height is bounded (60dvh) and its content scrolls. Mobile results match
the full field width. The shortcut hint disappears on mobile. Search exposes loading,
empty and failure text; result rows have minimum height (44px).
The mobile document selector is a native select with a white fill, border and minimum
height (44px). Sources: [Search.astro](src/components/Search.astro), [field styles](src/styles/global.css#L134),
[mobile selector](src/pages/docs/[...slug].astro#L23).

### Navigation

Header links change from muted to foreground on hover or current state. Desktop
document links have compact row padding (8px vertically, 10px horizontally);
hover and current page use a full surface fill. Current links also gain weight
(550). Sidebar links keep their compact text-row height; the 44px minimum applies
to buttons, header/footer actions, search results and the native mobile selector.
Adjacent-document navigation uses a top divider and two equal columns.
Sources: [navigation styles](src/styles/global.css#L114), [reader markup](src/pages/docs/[...slug].astro#L35).

### Markdown containers

Code blocks use a surface fill, uniform border, panel corners and internal horizontal
scrolling. The right inset reserves space for the copy action. Table rows use bottom
dividers; headers use surface, cells use muted text and the first column uses foreground.
Blockquotes use a surface fill and rounded corners without an accent rail. Native
disclosure containers use a uniform outline. Sources: [prose styles](src/styles/global.css#L87),
[Markdown renderer](src/lib/content.ts#L141).

**The Visible Focus Rule.** Keep the global keyboard outline visible (2px, offset 4px). Search may place that outline on the field wrapper; do not remove the wrapper treatment when suppressing the input outline.

## Do's and Don'ts

### Do:

- Do use the shared Geist and Geist Mono families.
- Do preserve Markdown structure, readable prose and contained code/table scrolling.
- Do indicate the current document with a full surface background.
- Do retain visible keyboard focus and native browser navigation gestures.

### Don't:

- Don't add decorative imagery, motion, shadows or gradients to this minimal system.
- Don't add decorative left-edge rails to navigation, quotes or containers.
- Don't invent a dark theme or an ornamental accent palette.
- Don't give body copy the landing title's display treatment.
