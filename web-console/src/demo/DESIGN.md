# QQ Maid Signal Glass Demo Design System

## 0. Research Log
- Embedded refs: shortlisted Linear, Supabase, and Vercel → picked soft-skill execution + Linear's dark-native hierarchy because the approved direction needs precise glass depth without rounded SaaS conventions.
- Lazyweb: skipped — this is an isolated approval artifact with no external product clone request.
- Imagen drafts: skipped — no image asset is required; the signature surface is authored as layered CSS glass and inline SVG.

## 1. Atmosphere & Identity
Signal Glass is a quiet operations cockpit: dark enough to feel instrument-like, bright enough to make state legible. Its signature is a liquid-glass plate suspended over a slow chromatic field, then machined into square modules with a 1px outer line, a 1px clear gap, and a 1px inner line.

## 2. Color

Every preset supplies the same semantic roles from three source colors: dark material, light canvas, and contrast accent. The light color owns the page field and whitespace; the dark color owns the glass/status surfaces; the contrast color owns readable text, icons, frames, and active signals.

| Role | Token | Default use |
|---|---|---|
| Material | `--demo-dark` | status bar and glass component background |
| Canvas | `--demo-light` | body, whitespace, and page field |
| Contrast | `--demo-contrast` | readable text, icons, frames, active navigation, status, focus, key numerals |
| Text primary | `--demo-text` | contrast text on dark material |
| Text muted | `--demo-muted` | derived contrast labels on dark material |
| Glass | `--demo-glass` | translucent dark material panel fill |
| Line outer | `--demo-line` | outer 1px frame |
| Line inner | `--demo-line-inner` | inset 1px frame |
| Good | `--demo-good` | healthy status |
| Warn | `--demo-warn` | attention state |

Named presets: `night-shift` (deep green / warm ivory / mint), `ember-grid` (charcoal / sand / ember), and `tide-signal` (deep teal / ice / coral). Preset source colors are exposed as `data-theme` CSS variable sets so switching is visible and inspectable.

## 3. Typography

- Primary: `ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif` — local/system only, no network dependency.
- Mono: `ui-monospace, SFMono-Regular, Menlo, Consolas, monospace`.

| Level | Size | Weight | Usage |
|---|---:|---:|---|
| Display | `clamp(2.25rem, 6vw, 5rem)` | 650 | hero state |
| H1 | `2rem` | 650 | page title |
| H2 | `1.125rem` | 650 | frame titles |
| Body | `0.9375rem` | 450 | reading text |
| Small | `0.75rem` | 550 | metadata |
| Overline | `0.625rem` | 700 | labels and nav |

## 4. Spacing & Layout

Base unit is 4px. Tokens are `--demo-space-1` through `--demo-space-12` (4px to 48px). The shell uses a 12-column desktop grid with a max width of 1280px and 24px gutters, collapsing to one column below 768px. The bottom navigation is fixed to the viewport and the main content owns vertical scrolling.

## 5. Components

### Glass Frame
- **Structure**: `article.demo-frame` with `::before` atmospheric sheen and a nested content region.
- **Variants**: `hero`, `metric`, `wide`, `settings`.
- **Spacing**: `--demo-space-3` to `--demo-space-6`.
- **States**: default, hover lift via transform, focus-visible when interactive.
- **Accessibility**: content remains semantic; interactive frames are buttons only when they perform an action.
- **Motion**: opacity/transform entry; disabled under reduced motion.
- **Layout**: grid item; no internal scroll.

### Status Item
- **Structure**: icon, label, value, status dot.
- **Variants**: healthy, pending, attention.
- **Spacing**: `--demo-space-2`.
- **States**: default and focus-visible when navigable.
- **Accessibility**: status is conveyed by text, not color alone.

### Bottom Navigation
- **Structure**: fixed `nav` with four labeled buttons and inline SVG icons.
- **Variants**: active view via `aria-current="page"`.
- **Spacing**: `--demo-space-2` and `--demo-space-3`.
- **States**: default, hover, active, focus-visible.
- **Motion**: active marker uses opacity/transform only.

## 6. Motion & Interaction

Micro interactions use 160ms `cubic-bezier(0.2, 0.8, 0.2, 1)` and view changes use 320ms `cubic-bezier(0.16, 1, 0.3, 1)`. Only `transform`, `opacity`, and filter-like atmospheric layers move. `prefers-reduced-motion: reduce` disables transitions and entry animation. Theme buttons update CSS variables synchronously; bottom navigation swaps local demo views without network work.

## 7. Depth & Surface

Strategy is mixed: translucent glass fill, backdrop blur on fixed/contained surfaces, a soft inset highlight, and a double-line frame composed of 1px outer stroke, 1px gap, and 1px inner stroke. No rounded corners and no heavy drop shadows. Atmospheric depth comes from fixed radial gradients and a slow, reduced-motion-safe glow.

## 8. Accessibility Constraints & Accepted Debt

- WCAG 2.2 AA target for text and controls; every action is a keyboard-focusable button with a visible focus ring.
- Navigation exposes `aria-current`; theme controls expose `aria-pressed`; status text is not color-only.
- Accepted debt: this is a static visual approval artifact, so settings controls are non-persisting and metrics are local sample data only. Production API integration belongs to a later approved frontend refactor.
