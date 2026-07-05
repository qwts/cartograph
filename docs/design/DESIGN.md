---
name: Cartograph
colors:
  surface: '#131313'
  surface-dim: '#131313'
  surface-bright: '#393939'
  surface-container-lowest: '#0e0e0e'
  surface-container-low: '#1c1b1b'
  surface-container: '#201f1f'
  surface-container-high: '#2a2a2a'
  surface-container-highest: '#353534'
  on-surface: '#e5e2e1'
  on-surface-variant: '#c1c6d5'
  inverse-surface: '#e5e2e1'
  inverse-on-surface: '#313030'
  outline: '#8b919f'
  outline-variant: '#414753'
  surface-tint: '#abc7ff'
  primary: '#abc7ff'
  on-primary: '#002f65'
  primary-container: '#448ffd'
  on-primary-container: '#002959'
  inverse-primary: '#005cba'
  secondary: '#7adaa1'
  on-secondary: '#003920'
  secondary-container: '#007848'
  on-secondary-container: '#9bfcc1'
  tertiary: '#ffb688'
  on-tertiary: '#512400'
  tertiary-container: '#e07316'
  on-tertiary-container: '#471f00'
  error: '#ffb4ab'
  on-error: '#690005'
  error-container: '#93000a'
  on-error-container: '#ffdad6'
  primary-fixed: '#d7e3ff'
  primary-fixed-dim: '#abc7ff'
  on-primary-fixed: '#001b3f'
  on-primary-fixed-variant: '#00458e'
  secondary-fixed: '#95f7bb'
  secondary-fixed-dim: '#7adaa1'
  on-secondary-fixed: '#002110'
  on-secondary-fixed-variant: '#005230'
  tertiary-fixed: '#ffdbc7'
  tertiary-fixed-dim: '#ffb688'
  on-tertiary-fixed: '#311300'
  on-tertiary-fixed-variant: '#733600'
  background: '#131313'
  on-background: '#e5e2e1'
  surface-variant: '#353534'
  tier-confirmed: '#27C93F'
  tier-inferred-strong: '#2D9CDB'
  tier-inferred-weak: '#F2C94C'
  tier-gap: '#EB5757'
  bg-surface: '#1A1A1A'
  bg-panel: '#242424'
  border-subtle: '#333333'
  text-muted: '#888888'
typography:
  headline-lg:
    fontFamily: Inter
    fontSize: 24px
    fontWeight: '600'
    lineHeight: 32px
  headline-md:
    fontFamily: Inter
    fontSize: 18px
    fontWeight: '600'
    lineHeight: 24px
  body-md:
    fontFamily: Inter
    fontSize: 14px
    fontWeight: '400'
    lineHeight: 20px
  body-sm:
    fontFamily: Inter
    fontSize: 12px
    fontWeight: '400'
    lineHeight: 16px
  mono-code:
    fontFamily: JetBrains Mono
    fontSize: 13px
    fontWeight: '400'
    lineHeight: 20px
  label-caps:
    fontFamily: Inter
    fontSize: 11px
    fontWeight: '700'
    lineHeight: 14px
    letterSpacing: 0.05em
rounded:
  sm: 0.125rem
  DEFAULT: 0.25rem
  md: 0.375rem
  lg: 0.5rem
  xl: 0.75rem
  full: 9999px
spacing:
  sidebar-width: 240px
  narrow-sidebar: 64px
  gutter: 16px
  panel-padding: 12px
  stack-gap: 8px
---

## Brand & Style

The design system embodies **Technical Rigor and Absolute Transparency**. As a developer tool for high-stakes system mapping, it rejects decorative flourishes in favor of "Integrity-first" aesthetics. The brand personality is authoritative, precise, and honest—prioritizing the distinction between "Confirmed" facts and "Inferred" hypotheses.

The chosen style is **Modern Corporate / Minimalist**, heavily influenced by the **native macOS desktop experience**. It utilizes a sophisticated dark theme with a "Glassmorphic" influence—not for flair, but to provide depth and context within a complex, multi-pane workspace. The UI should feel like a high-end IDE: dense with information but perfectly organized through clear hierarchy and a "Command Palette" first interaction model.

## Colors

The palette is functionally driven by the "Escalation Ladder" of data confidence.

- **Primary Canvas**: A deep charcoal background (`#121212`) ensures high contrast for data visualization. 
- **Functional Tiers**: 
    - **Confirmed (Green)**: Indicates deterministic truth (T0/T1).
    - **Inferred Strong (Blue)**: Indicates high-confidence semantic matches.
    - **Inferred Weak (Yellow)**: Indicates agentic guesses requiring human review.
    - **Gap (Red)**: Highlights unresolved hops and missing information.
- **Layering**: Surfaces use subtle variations of slate gray to define hierarchy without needing heavy borders. Text utilizes a "vibrant" white for headings and a muted gray for metadata to reduce visual fatigue during long sessions.

## Typography

The typography system balances readability and data density. **Inter** is the primary typeface for its exceptional legibility at small sizes and its neutral, professional tone. **JetBrains Mono** is reserved for technical data, hashes, and code-level provenance, providing the necessary distinction for technical facts.

- **Headlines**: Semi-bold and compact.
- **Data Density**: The default body size is 14px, but the "Evidence Panel" and "Atlas Sidebar" may drop to 12px to maximize information display.
- **Technical Provenance**: Every `content_hash` or `extractor_id` must be rendered in `mono-code` to signify its deterministic nature.
- **Badges**: Tier labels use `label-caps` for immediate recognition in the sequence views.

## Layout & Spacing

This design system uses a **Fixed-Fluid Hybrid Layout** optimized for desktop productivity.

- **The Shell**: A persistent 64px narrow sidebar for primary navigation (Atlas, Flow, Spec, Jobs), expanding to 240px when hovered or pinned.
- **The Top Bar**: A centered, translucent "Command Palette" bar (`Cmd+K`) that floats above the content.
- **The Canvas**: The main viewport is a fluid surface for Cytoscape.js or React Flow visualizations.
- **The Evidence Panel**: A 320px fixed-width right drawer that slides in to provide context for selected nodes.
- **Grid**: A strict 4px/8px baseline grid maintains alignment across dense technical tables and property inspectors.

## Elevation & Depth

Elevation is conveyed through **Tonal Layers** and **Subtle Glassmorphism** to mimic the macOS aesthetic.

- **Base Layer**: The background is `#121212`.
- **Primary Panels**: Floating sidebars and the Command Palette use `#1A1A1A` with a subtle 20px backdrop blur and a 1px `#333333` border.
- **Contextual Popovers**: Use a 15% opacity tint of the primary color in the shadow to indicate active focus.
- **Shadows**: Shadows are avoided in the primary layout to keep the UI "flat" and fast, but are used for high-level overlays (modals) with a 24px blur and 0.4 opacity.

## Shapes

The shape language is **Soft (0.25rem)**, prioritizing a modern, tool-like feel that avoids the playfulness of fully rounded corners.

- **Buttons & Inputs**: 4px radius (`rounded-sm`).
- **Cards & Panels**: 8px radius (`rounded-lg`).
- **Tier Badges**: 2px radius or full-pill depending on context (e.g., pill for status tags, small radius for node indicators).
- **Graph Nodes**: Nodes use distinct geometric shapes—rectangles for Services, diamonds for Gateways, and octagons for Gap nodes—to provide instant visual categorization.

## Components

- **Buttons**: Primary buttons use a subtle gradient of the brand blue. Secondary buttons are "Ghost" style (transparent background, 1px border). 
- **Tier Badges**: Compact labels with a background color corresponding to the Confidence Tier. Inferred items must always include a "source" icon.
- **Input Fields**: Dark backgrounds with a 1px border that glows primary blue on focus. Use monospaced fonts for IDs and SHA inputs.
- **Cards (Flow Inspector)**: Sequenced vertically. "Gap Cards" are highlighted with a dashed red border and a warning icon to signify a break in the deterministic chain.
- **The Evidence Panel**: A vertical stack of metadata. Each entry includes a "Jump to Source" button that opens the read-only code viewer at the specific span.
- **Job Progress**: A thin, non-intrusive progress bar at the very top of the viewport for long-running Rust core ingestions.