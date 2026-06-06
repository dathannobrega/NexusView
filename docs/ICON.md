# NexusView — App Icon Design Brief

A precise spec a designer (or an image model) can execute. The icon must read at
**1024 px down to 16 px**, follow the **macOS (Big Sur+) icon language**, and
signal the product: a fast, native **DFIR / SOC timeline-triage** tool.

---

## 1. Format & frame (macOS rules)

- **Shape:** the standard macOS **rounded-rectangle "squircle"** (super-ellipse),
  **not** a circle and **not** a full-bleed square. Approx. **824×824 px** of
  artwork centered inside the **1024×1024 px** canvas, leaving the system margin
  (~100 px each side). Corner radius ≈ **185 px** at 1024.
- **Depth, not skeuomorphism:** a single subtle top-down light, soft contact
  shadow under floating elements, gentle inner highlight on the top edge of the
  squircle. Flat-modern with a hint of dimensionality — like Xcode, Console, or
  Numbers. No heavy bevels, no glossy "web 2.0" gloss, no drop-shadowed text.
- **Deliverables:** full `AppIcon.appiconset` — 16, 32, 64, 128, 256, 512, 1024
  (each @1x and @2x). Export a flat 1024 master PNG (sRGB) + a layered source
  (SVG/Sketch/Figma). The build can package these into `NexusView.app/Contents/
  Resources/AppIcon.icns`.

---

## 2. Concept (primary) — "The lens over the timeline"

The strongest, most legible idea. Two instantly-readable forensic symbols fused:

- **Background tile:** the squircle filled with a smooth **diagonal gradient**
  from deep indigo/navy at the top-left to a darker near-black blue at the
  bottom-right — a calm "night-ops / SOC console" feel. Optional very faint
  vertical grid lines (2–3% opacity) evoking a data table.
- **The data (mid layer):** **3–4 stacked horizontal "timeline lanes"** —
  rounded rectangles like rows in a grid — receding slightly in perspective.
  Most are a cool neutral slate. **Exactly one lane is highlighted in alert
  red/orange** (the anomaly the analyst is hunting). This single warm accent
  against the cool field is the focal point and the whole story: *find the one
  bad event in the timeline.*
- **The lens (top layer):** a clean **magnifier** floating over the lanes,
  bottom-left to top-right. Thin **electric-cyan/teal rim**, glass rendered as a
  subtle frosted circle that **brightens the lanes inside it** (slightly higher
  contrast + a faint cyan tint), with a short handle. Inside the glass, the
  highlighted red lane is clearly visible — magnified.
- **Result:** "view the timeline → spot the threat." Reads at 16 px as
  *lens + colored bars*; rewards inspection at 1024 px with the grid texture and
  glass refraction.

### Composition
- Magnifier glass centered slightly up-left, ~46% of the tile width.
- Timeline lanes span ~70% width, centered, the red one second from top.
- Keep a clear silhouette — the lens circle + handle must survive at tiny sizes.

---

## 3. Concept (alternate) — "The nexus node"

If a more abstract, brand-forward mark is preferred:

- Same indigo→navy squircle.
- A central **hexagonal node** (the "nexus") in cyan, with **thin connecting
  lines radiating to 4–5 smaller satellite nodes** (correlation / graph), laid
  over faint horizontal timeline rows. **One satellite node is red** (the IOC).
- More "platform/automation" (nods to the MCP/correlation features), but less
  literal than the lens. Use this only if the lens concept feels too utilitarian.

---

## 4. Color palette

| Role | Color | Hex |
|---|---|---|
| BG gradient top-left | Indigo | `#2A3A8C` |
| BG gradient bottom-right | Deep navy/near-black | `#0B1020` |
| Timeline lanes (neutral) | Slate | `#5B6B86` |
| **Alert lane / IOC node** | Signal red-orange | `#FF5A3C` |
| Lens rim / accents / glass tint | Electric cyan | `#27E0C8` |
| Highlights / inner edge | Soft white | `#EAF2FF` @ 60–80% |

Keep the palette tight: **one cool field (indigo/cyan) + one warm accent
(red-orange).** The warm accent must appear **once** so it stays the focal point.

---

## 5. Do / Don't

**Do**
- Keep one dominant silhouette (the lens) legible at 16 px.
- Use the single red accent as the "story."
- Align to the macOS grid; test on light *and* dark Dock backgrounds.

**Don't**
- No letters/words baked into the icon ("NexusView" text, "N" monograms are weak
  at small sizes — the alternate concept's node is fine, literal letters are not).
- No magnifying-glass cliché *alone* (it must reveal the timeline, not float over
  nothing).
- No thin 1 px details that vanish when downscaled; no photographic textures; no
  pure-black or pure-white fills (use the near-navy and soft-white above).
- No circular badge frame — respect the squircle.

---

## 6. One-line prompt (for an image model)

> macOS app icon, rounded-square squircle, indigo-to-navy gradient background,
> a frosted magnifier with a thin electric-cyan rim floating over 3–4 stacked
> horizontal timeline bars, one bar glowing alert red-orange and magnified inside
> the glass, soft top light, subtle depth, flat-modern, crisp at small sizes,
> centered, no text.
