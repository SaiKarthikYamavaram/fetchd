# spool — Modern, Elegant & Aesthetic UI Overhaul Roadmap

This document specifies the design, visual architecture, and component overhaul to elevate **spool** into a modern, elegant, aesthetic, and desktop-native internet download manager inspired by Linear, Raycast, Arc, and modern macOS design principles.

---

## 1. Executive Summary & Design Vision

### Current Pain Points
- **Constrained Layout**: The application viewport is confined to a narrow, centered column (`max-w-3xl`) within a 940×760 window, leaving dead margins on wide displays and crowding controls into cramped rows.
- **Visual Hierarchy & Depth**: The UI relies on flat borders and basic cards without subtle depth, ambient lighting, or visual grouping.
- **Status & Category Discoverability**: Filtering is restricted to three tabs (`All`, `Active`, `Done`), lacking media categorization (Video, Audio, Docs, Archives) and quick-access status counts.
- **Transfer Inspection**: Progress bars are monolithic and lack visual multi-segment feedback (the hallmark feature of high-performance download managers).
- **Typography & Metrics**: Speed and byte numbers jitter during high-frequency updates without tabular figure alignment (`tabular-nums font-mono`).

### Target Experience
- **Desktop-Native App Shell**: Collapsible sidebar with status queues, media category buckets, live network HUD, theme selector, and settings access.
- **Fluid & Responsive Dashboard**: Full-width glassmorphic surface with smooth transitions, backdrop blur (`backdrop-blur-xl`), hairline gradient borders, and ambient light meshes.
- **Live Segment Visualizer**: Dedicated multi-thread progress visualizer in the download inspector showing live chunks across parallel connection sockets.
- **Typographic Precision**: Inter font-family with proper antialiasing, balanced hierarchy, and rock-solid `tabular-nums` for real-time telemetry.

---

## 2. Design System & Aesthetics Specification

### 2.1 Typography & Smoothing
- **Font Stack**: `'Inter', -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif`
- **Smoothing**: `-webkit-font-smoothing: antialiased; -moz-osx-font-smoothing: grayscale;`
- **Telemetry Figures**: `font-mono tabular-nums text-xs font-medium` for transfer speeds, ETA, downloaded bytes, and connection counts to eliminate layout shift during live updates.

### 2.2 Color & Surface Palette (OKLCH / HSL)
- **Dark Mode (Default)**:
  - Deep Obsidian Canvas: `oklch(0.12 0.012 260)` (`#090b10`)
  - Translucent Surface Cards: `oklch(0.16 0.015 260 / 80%)` with `backdrop-blur-xl`
  - Subtle Hairline Borders: `oklch(1 0 0 / 8%)` with hover state `oklch(1 0 0 / 18%)`
  - Primary Glow Accent: Luminous electric violet & indigo `oklch(0.65 0.2 275)`
  - Status Indicators:
    - *Downloading*: Vivid Violet / Cyan gradient (`from-violet-500 via-indigo-500 to-cyan-400`) with pulsing activity dot
    - *Completed*: Vibrant Emerald `oklch(0.72 0.17 155)` with soft green halo
    - *Paused*: Warm Amber `oklch(0.78 0.16 75)`
    - *Failed*: Rose Red `oklch(0.65 0.22 25)`
- **Light Mode**:
  - Crisp Porcelain Canvas: `oklch(0.985 0.003 260)` (`#f8fafc`)
  - Elevated Cards: Pure White `oklch(1 0 0)` with soft border and diffused shadow
  - Deep Violet Accent: `oklch(0.5 0.22 275)`

### 2.3 Micro-interactions & Motion
- **Progress Shimmer**: Fluid wave animation on indeterminate downloads instead of jarring opacity blinks.
- **Card Transitions**: Smooth hover elevation (`translate-y-[-1px]`), active press states, and glowing border highlights.
- **Scrollbars**: Ultra-slim 6px translucent rounded pills that blend seamlessly into the background.

---

## 3. Structural & Component Architecture

### 3.1 Application Shell (`App.tsx`)
The interface transitions to a two-pane desktop layout:

```
┌─────────────────┬────────────────────────────────────────────────────────┐
│  SPOOL BRAND    │ [Search downloads...     ⌘F]   [Import] [+ Add Download]│
├─────────────────┼────────────────────────────────────────────────────────┤
│ QUEUES          │ Title: Active Downloads (3 items · 14.8 MB/s)          │
│ • All        12 │ [Resume All] [Pause All] [Select Mode] [Clear Done]    │
│ • Active      3 ├────────────────────────────────────────────────────────┤
│ • Completed   8 │ ┌────────────────────────────────────────────────────┐ │
│ • Paused      1 │ │ [Video] Ubuntu 24.04 LTS        [releases.ubuntu]  │ │
│                 │ │ ▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓░░░░░░░░░░░░░░░░░░░░  48%     │ │
│ CATEGORIES      │ │ 1.8 GB / 4.2 GB · ▲ 8.4 MB/s · ⏱ 3m 45s · 8 conns │ │
│ • Media       6 │ └────────────────────────────────────────────────────┘ │
│ • Documents   3 │ ┌────────────────────────────────────────────────────┐ │
│ • Archives    2 │ │ [Archive] archive.tar.gz         [github.com]      │ │
│ • Other       1 │ │ ▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓▓ 100%     │ │
├─────────────────┤ │ 512 MB · Completed · Open File · Show Folder       │ │
│ NETWORK HUD     │ └────────────────────────────────────────────────────┘ │
│ ▲ 14.8 MB/s     │                                                        │
│ [Theme] [Pref]  │                                                        │
└─────────────────┴────────────────────────────────────────────────────────┘
```

#### A. Left Navigation Sidebar
1. **Brand Bar**: Glowing Spool ring glyph, styled typographic wordmark, and connection status indicator.
2. **Queue Filters**:
   - *All Downloads* (with total badge)
   - *Active* (with live animated pulse dot and running count)
   - *Completed* (with checkmark and finished count)
   - *Paused* (with pause badge)
   - *Failed* (with alert badge)
3. **Category Groups** (driven by `lib/filetype.ts`):
   - *Media* (Videos & Audio)
   - *Documents* (PDF, Word, Sheets, Books)
   - *Archives & Packages* (Zip, Tar, Deb, Dmg, Exe)
   - *Other Files*
4. **Bottom HUD & Utilities**:
   - Live network speed meter (`▲ 14.8 MB/s`) with dynamic activity pulses.
   - Quick theme cycler (System / Dark / Light) with visual icons.
   - Settings toggle.

#### B. Main Workspace & Top Command Bar
1. **Omni-Search Bar**: Centered or prominent glassmorphic input with keyboard shortcut pill (`⌘F` / `Ctrl+F`), autofocus, and clear button.
2. **Action Bar**:
   - Global queue controls: **Pause All**, **Resume All**, **Select Mode**, and **Clear Finished**.
   - Primary action buttons: **Import** button and glowing **+ Add Download** gradient button (`from-violet-600 to-indigo-600`).
3. **Batch Selection Ribbon**:
   - Floating glass banner appearing upon selection.
   - Selection count pill (`3 selected`), Select All, Pause, Resume, Delete, and Exit.

---

## 4. Component Details

### 4.1 Download Queue Card (`Row`)
- **Visual File Badge**:
  - Distinct rounded squircle tile color-coded by file kind (video: violet, audio: rose, doc: blue, archive: amber, code: emerald).
  - Video preview thumbnail support with live circular progress ring (`RingProgress`).
- **Domain Origin Pill**:
  - Extracted hostname pill (e.g. `github.com`, `youtube.com`) next to filename for immediate provenance.
- **Gradient Progress Track**:
  - Dual-tone gradient indicator (`from-violet-500 via-indigo-500 to-cyan-400` while downloading; vibrant emerald upon completion).
  - Smooth flowing shimmer wave for indeterminate transfers.
- **Telemetry Line (`tabular-nums font-mono`)**:
  - Left: Downloaded size / Total size (`1.4 GB / 3.8 GB · 37%`).
  - Right: Transfer rate (`▲ 6.2 MB/s`), remaining time ETA (`⏱ 4m 12s left`), segment sockets (`⚡ 8 conns` / `yt-dlp`).
- **Contextual Hover Actions**:
  - One-click buttons for Pause, Resume, Retry, Open File, Show in Folder, and More Menu (`...`).

### 4.2 Detail Inspector (`DetailModal.tsx`)
- **Multi-Segment Visualizer**:
  - Live horizontal connection thread bars visualizing each segment's byte range (`[start, end]`), downloaded fraction, and real-time chunk progress.
- **Properties & Headers**:
  - Copyable chip for destination path and source URL.
  - Engine tag (`HTTP Multi-Segment` vs `yt-dlp`), byte-range support verification, creation date.
  - Browser session metadata (cookies, User-Agent, referer).

### 4.3 Add & Import Dialog (`AddDialog.tsx`)
- Tabbed selector: "Single Download" vs "Batch Import".
- Automatic video detection: highlights recognized video platforms with a video badge and exposes the quality selector (4K, 1440p, 1080p, 720p, Audio MP3).
- Path selector with clean breadcrumb path and native Browse button.
- "Start immediately" toggle switch.

### 4.4 Settings Panel (`SettingsView.tsx`)
- Visual Theme Selector cards (System, Light, Dark) with mockup icons for 1-click theme switching.
- Bandwidth speed limit presets (Unlimited, 1 MB/s, 5 MB/s, 10 MB/s, Custom) with KB/s input.
- Debounced configuration writes and immediate feedback badge.

---

## 5. Verification & Implementation Plan

### Automated Test Suite
- Ensure all 89 unit tests pass:
  ```bash
  npm test
  ```

### Build & Typecheck
- Validate TypeScript definitions and production asset bundle:
  ```bash
  npm run build
  ```

### Functional & Visual Validation
1. Verify both Dark and Light themes render with appropriate contrast, glassmorphism, and borders.
2. Validate sidebar navigation: clicking queues (All, Active, Completed, Paused, Failed) and category filters (Media, Documents, Archives, Other) filters accurately.
3. Test batch selection mode, keyboard shortcuts (`⌘F`, `Escape`, `⌘A`), and URL addition.
4. Verify real-time speed readout stability and segment visualizer in DetailModal.
