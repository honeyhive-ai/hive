# Hive Logo System

## Summary

Hive now has an HTML-driven branding source and generated asset set so the mark can be reused across macOS, Linux, Windows, web, and future mobile packaging without redrawing it per client.

Primary assets:

- `assets/branding/HiveLogo.html`
- `assets/branding/generate-brand-assets.mjs`
- `assets/branding/render-tauri-icons.sh`
- `assets/branding/hive-app-icon.svg`
- `assets/branding/hive-mark.svg`
- `assets/branding/hive-mark-mono.svg`
- `assets/branding/hive-tray-template.svg`
- `assets/branding/hive-tray-template-128.png`
- `assets/branding/hive-lockup.svg`
- `assets/branding/hive-brand-tokens.json`
- `web/src/components/HiveBrand.tsx`

## Design direction

The mark is intentionally calm and utilitarian rather than mascot-like or overly playful.

The source concept in `HiveLogo.html` defines a seven-cell honeycomb rosette:

- one lit center chamber
- two additional lit chambers in an upward lean
- four quieter surrounding chambers
- a warm comb-tile app icon surface

This is the first branding pass in the repo that derives the actual source assets from the authored concept instead of a hand-transcribed approximation.

## Technical design

### 1. Geometry

The generated mark and icon follow the HTML source logic:

- hexes are point-up and generated from one radius constant
- the mark is a seven-cell rosette: center plus six ring cells
- active chambers are the center, right, and up-left cells
- the app icon places the rosette inside a rounded-square tile with a honey-brown gradient
- the standalone mark remains transparent for docs and UI embedding

### 2. Color system

The palette is derived from the HTML branding source and then mapped into the app theme tokens:

- canvas: `#F4F1EA`
- ink: `#221F1A`
- honey comb: `#F0C25A` → `#C8881F`
- warm accent: `#E3A460` → `#C6713A`
- tile gradient: `#9A6620` → `#4A2E12`
- inactive cream cells: `#FBF1D8`, `#F0E0B8`

These values now inform the default web shell theme as well, so the product chrome and the app mark speak the same visual language.

### 3. Cross-platform compatibility

SVG is the source of truth because it is:

- resolution-independent
- easy to rasterize into platform app-icon sets later
- usable in web/Tauri immediately
- neutral across Swift, React, and Rust packaging flows

The React brand component mirrors the SVG geometry directly so the web shell does not depend on a loader-specific SVG import path.

The shell keeps the brand icon inline as JSX for portability, while the generated SVG files remain the canonical export assets for docs and packaging flows.

## Intended usage

Use `hive-mark.svg` for:

- app icon source generation
- sidebar/app badges
- settings/about surfaces
- installer and repository branding

Use `hive-lockup.svg` for:

- README / docs headers
- release notes
- website or landing-page branding

- `hive-app-icon.svg` is the source for raster app-icon generation.
- `hive-tray-template.svg` and `hive-tray-template-128.png` are the tray-only source assets for the macOS menubar icon.

## macOS tray technical note

The app icon and the tray icon intentionally diverge:

- the app icon keeps the warm rounded-square tile
- the tray icon uses a transparent monochrome rosette so macOS can tint it as a template image

`render-tauri-icons.sh` still uses Quick Look to rasterize the full app icon SVG, but the tray icon is rendered directly by `generate-brand-assets.mjs` into a transparent PNG. This avoids Quick Look flattening the tray SVG onto an opaque white background, which made the menubar item look too large and visually boxed-in.

## Follow-up work

- refresh the remaining Windows `.ico` artifact from the generated PNG base
- decide whether Android/iOS adaptive icons should use the full tile or the transparent rosette foreground
