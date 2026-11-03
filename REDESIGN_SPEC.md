# tri7 Desktop Wallet Redesign Specification

## 1. Design mandate

This redesign is **the historical Cryptographic Triangles Qt wallet, rebuilt cleanly for egui**. It is not a generic crypto dashboard and it is not a visual rebrand.

Preserve these identity anchors:

- black application shell;
- red-orange triangle mark and `#e32105` brand accent;
- strong horizontal header, left navigation, central work area, and bottom network/status strip;
- original primary page order and original wallet terminology;
- compact, tool-like information density;
- red/brown/gold palette DNA from the supplied wordmarks.

Modernize these aspects:

- use crisp vector/code-drawn geometry or clean high-resolution PNGs, not scaled 2013 bitmaps;
- use flat fills, precise 1 px borders, restrained rounding, and no bevels or fake gloss;
- make body copy neutral and legible instead of painting every label bright red;
- establish consistent spacing, alignment, interaction states, tables, and validation;
- keep technical truth and all existing functions, but move diagnostic detail behind disclosure controls.

The target impression is **a serious black-and-red desktop wallet with unmistakable Triangles lineage**, not a web app. Default minimum window size: **1040 × 700 logical px**; preferred initial size: **1180 × 780**.

## 2. Source-of-truth observations

The historical `forms/mainwindow.ui` establishes:

- `#000000` main window and central background;
- `#e32105` global accent and separator;
- `#1c1c1c` combo/input surfaces;
- `#3d0e04` navigation hover/pressed and list selection;
- `#AF272E` menu background, `#deeef1` menu text, `#9D0B17` selected menu item, and `#E9D9D8` selected menu text;
- `#AB7F82` legacy header-role surface;
- `#e0e0e0` legacy disabled input surface;
- original primary sidebar order: **Overview, Send Coins, Receive Coins, Address Book, Transactions, Messages**;
- original bottom status area height: **52 px**, with connection, synchronization, staking, and encryption state;
- original content concepts: Wallet balance frame, Recent transactions, Coin Control Features, request-payment QR, address book, transaction filters/table, encrypted Messages, tabbed Options, and About Cryptographic Triangles.

The brand references establish two complementary lockups:

- `brand/triangles-wordmark-wide.jpg`: a red-orange triangle plus broad, heavy “Triangles” display lettering with warm copper/gold highlights and dark red-brown depth;
- `brand/cryptographic-triangles-compact.jpg`: a flat red triangle plus stacked red “Cryptographic / Triangles” lettering on black.

The wide reference has a white photographic/JPEG field and the compact reference has a black field. Neither JPEG should be placed directly into the production UI. They are reconstruction references.

## 3. Exact production palette

Use these constants throughout. Values marked “historical exact” come directly from the Qt stylesheet; brand-derived values are representative dominant colors sampled from the supplied references and normalized for UI use.

| Token | Hex | Use | Provenance |
|---|---:|---|---|
| `WINDOW_BG` | `#000000` | app shell, sidebar, header, status bar | historical exact |
| `CONTENT_BG` | `#090909` | central page canvas; separates content from pure-black chrome | modern derived |
| `PANEL_BG` | `#121212` | cards, dialogs, table body | modern bridge |
| `INPUT_BG` | `#1C1C1C` | inputs, combos, code/hex areas | historical exact |
| `ROW_ALT` | `#17110F` | alternate table row | palette-derived |
| `BORDER` | `#5B2013` | ordinary panel/control border | sampled red-brown |
| `BORDER_SUBTLE` | `#2D1712` | dividers and inactive outlines | derived |
| `BRAND_RED` | `#E32105` | primary action, active marker, logo, focus | historical exact; compact sample is approximately `#E32206` |
| `BRAND_RED_HOVER` | `#F03A18` | primary-button hover only | modern derived |
| `BRAND_RED_PRESSED` | `#C51B04` | primary-button pressed | modern derived |
| `DEEP_RED` | `#3D0E04` | nav hover/selection surface, selected rows | historical exact |
| `MENU_RED` | `#AF272E` | menu/dialog accent surface where historical reference matters | historical exact |
| `MENU_SELECTED` | `#9D0B17` | selected menu item | historical exact |
| `HEADER_MAUVE` | `#AB7F82` | reference only; do not use as a large header fill | historical exact |
| `COPPER` | `#A65430` | wordmark depth, tertiary data highlight | sampled wide wordmark |
| `WARM_GOLD` | `#D36532` | wordmark highlight, tiny decorative accents only | sampled wide wordmark |
| `TEXT_PRIMARY` | `#F2E9E6` | primary body text and values | modernized warm white |
| `TEXT_SECONDARY` | `#C8B7B2` | descriptions, metadata | modernized warm gray |
| `TEXT_MUTED` | `#8F7770` | hints, timestamps, disabled labels | modernized muted tier |
| `MENU_TEXT` | `#DEEEF1` | menu text | historical exact |
| `MENU_SELECTED_TEXT` | `#E9D9D8` | selected menu text | historical exact |
| `DISABLED_BG` | `#161616` | disabled control background | modern replacement for legacy light gray |
| `DISABLED_FG` | `#66534E` | disabled text/icon | modern replacement |
| `SUCCESS` | `#55CC66` | synced, confirmed, broadcast success, staking active | retain current semantic color |
| `TX_IN_GREEN` | `#3FA34D` | received-coins amounts, primary token | Sami directive — received-coins color |
| `TX_IN_GREEN_BRIGHT` | `#4CBB6C` | received-coins amounts, bright text variant on black surfaces | Sami directive — received-coins color |
| `TX_IN_GREEN_DARK` | `#2E7D3B` | received-coins green, darker variant on lighter surfaces | Sami directive — received-coins color |
| `WARNING` | `#E0A33A` | syncing, immature, caution | new semantic color tied to wordmark warmth |
| `ERROR` | `#F05A4A` | validation and failure only | modern semantic red distinct from brand red |
| `INFO` | `#77AADD` | informational/transport state | retain current semantic family |

Rules:

- Do **not** set `override_text_color` to `BRAND_RED`. `TEXT_PRIMARY` is the global default.
- Red is for identity and action: active navigation, headings/eyebrows, focus, borders, and primary buttons.
- Never use `#E0E0E0` as a disabled background in the dark theme; it is recorded for provenance only.
- Do not use gradients in controls or panels. The only permitted tonal blend is inside the reconstructed display wordmark, and even there it must be subtle.
- Maintain at least 4.5:1 contrast for ordinary text. Never place small `BRAND_RED` text directly on `DEEP_RED`.

## 4. Typography and wordmark

### 4.1 UI type

Use egui’s bundled proportional font initially, but configure explicit text styles so a future bundled family can be swapped without layout changes. Preferred bundled upgrade: **Inter** or **Noto Sans** (regular 400, medium 500, semibold 600, bold 700). Use a bundled monospace such as **JetBrains Mono** or **Noto Sans Mono** for addresses, hashes, raw transaction hex, heights, and numeric debug output.

| Role | Size | Weight/color | Use |
|---|---:|---|---|
| Display wordmark | 27–30 px | custom asset | header only |
| Page title | 25 px | 600, `TEXT_PRIMARY` | one per page |
| Page subtitle | 13 px | 400, `TEXT_SECONDARY` | directly under title |
| Section title | 15 px | 600, `TEXT_PRIMARY` | cards/table sections |
| Body | 14 px | 400, `TEXT_PRIMARY` | controls and normal copy |
| Small/meta | 12 px | 400, `TEXT_SECONDARY` | timestamps, help, sources |
| Micro/status | 11 px | 500, `TEXT_MUTED` | bottom status strip |
| Balance value | 22 px | 600, `TEXT_PRIMARY` | overview totals |
| Monospace | 12.5 px | 400 | hashes, addresses, hex |

Use sentence case. Preserve ampersand accelerators in action labels only if eframe keyboard activation is actually implemented; otherwise render the visible label without `&` and add explicit shortcuts/tooltips.

### 4.2 Header brand lockup

Reconstruct, do not upscale, `header_logo.png` or either JPEG.

- Mark: clean equilateral red-orange triangle, 38 × 34 px, flat `BRAND_RED`, with an optional 1 px dark-copper lower-edge accent. No blur, bevel, glow, or shadow.
- Wordmark: “Triangles” in a heavy, wide display face. Preferred execution is a carefully traced SVG/transparent 2× PNG based on the wide reference. Use a restrained top-to-bottom highlight from `WARM_GOLD` into `COPPER`, with a dark `#300805` 1 px lower/outer keyline. If a clean asset cannot be produced, use flat `TEXT_PRIMARY` lettering with a `BRAND_RED` eyebrow rather than imitating a raster gradient in code.
- Small descriptor: `CRYPTOGRAPHIC` in 10 px semibold uppercase, tracking about 1.8 px, `BRAND_RED`, above the first half of “Triangles.” This connects the compact lockup with the wide wordmark.
- Header lockup size: approximately 220 × 46 px including mark and wordmark. Left inset 20 px.
- About lockup: use the compact stacked composition—triangle at left, `Cryptographic` / `Triangles` on two lines—reconstructed on transparency at 2×.

## 5. Application shell

### Current versus new

| Area | Current `app.rs` | Required redesign |
|---|---|---|
| Global text | all text forced to red | warm-white body text; red reserved for brand/action |
| Header | legacy 300×63 raster squeezed to 150×49; File/Operations/Help on right | crisp 220×46 lockup, 64 px header, File/Settings/Help menus, subtle bottom red rule |
| Sidebar | 190 px; icon and button are separate widgets; 11 equal nav rows | 216 px; icon and label share one full-row hit target; primary six pages first, utility pages grouped below |
| Selection | only button sub-rectangle gets `DEEP_RED` | full-width `DEEP_RED`, 3 px red left rail, bright text/icon |
| Content | pure black and generic vertical scroll | `CONTENT_BG`, 24 px page padding, consistent page header and cards |
| Status | one crowded horizontal line plus duplicate sidebar state | dedicated 40 px status strip with icon clusters, flexible sync region, no duplicate sidebar indicators |
| Cards/tables | ad hoc frames and grids | shared card/table primitives with consistent borders, headers, row heights |

### Geometry

- Header: 64 px high, `WINDOW_BG`; 1 px `BRAND_RED` bottom rule at 35% opacity. Brand left; menu bar right. Do not recreate custom window minimize/close controls unless the app uses a frameless native window.
- Sidebar: 216 px, `WINDOW_BG`, right border `BORDER_SUBTLE`. 12 px outer padding. Navigation row 44 px high, 8 px vertical gap only between groups.
- Content: `CONTENT_BG`; 24 px outer padding; maximum readable page width 1120 px but tables may fill all available width.
- Status bar: 40 px, `WINDOW_BG`, 1 px `BORDER_SUBTLE` top border.
- Dialogs: `PANEL_BG`, 1 px `BORDER`, 8 px corner radius, 20 px content padding; never pure black floating on pure black without a border.

## 6. Sidebar and exact order

The historical primary order must remain exact at the top:

1. Overview — `menu_16/overview`
2. Send Coins — `menu_16/send`
3. Receive Coins — `menu_16/receive`
4. Address Book — `menu_16/addressbook`
5. Transactions — `menu_16/transactions`
6. Messages — `menu_16/messages`

Then a 1 px divider and `TOOLS` group label, followed by current tri7 additions:

7. Staking — use a newly cleaned navigation staking icon based on `icons/staking_on/off`
8. Block Explorer — `menu_16/search`
9. Sign Message — `menu_16/sign`
10. Verify Message — `menu_16/verify`

Place **Options** at the bottom of the sidebar, separated from page navigation, using `menu_16/options`. This preserves every current page while restoring the historical primary hierarchy. `Screen::NAV_ORDER` may remain the routing source, but rendering must explicitly group it rather than displaying eleven undifferentiated rows.

Navigation row specification:

- 192 × 40 px inside the sidebar; corner radius 4 px;
- icon 20 × 20 px at x=12, label starts x=44;
- default: transparent background, `TEXT_SECONDARY` label;
- hover: `DEEP_RED`, `TEXT_PRIMARY`, no border;
- selected: `DEEP_RED`, 3 px `BRAND_RED` left rail, `TEXT_PRIMARY` semibold;
- keyboard focus: 1 px `BRAND_RED` inner stroke;
- disabled: `DISABLED_FG`, no hover fill;
- Messages may show an unread badge: minimum 18 px pill, `BRAND_RED` fill, black text.

## 7. Shared components

### Page header

Every page begins with title + one-line purpose text, then 20 px gap. Do not use a generic `ui.heading()` followed by a default separator. A 24 px red rule (2 px high) beneath the title is the recurring historical accent.

### Cards

`PANEL_BG`, 1 px `BORDER_SUBTLE`, radius 6 px, 16 px padding. Section title at top; optional right-aligned action. Hoverable cards may lift only by border brightening to `BORDER`; no shadows.

### Inputs

- 36 px standard height, `INPUT_BG`, radius 4 px;
- 1 px `BORDER`; hover `#7A2B19`; focus 1.5 px `BRAND_RED`;
- horizontal padding 10 px;
- placeholder `TEXT_MUTED`, value `TEXT_PRIMARY`;
- invalid border `ERROR` and a 12 px inline error below;
- disabled `DISABLED_BG` / `DISABLED_FG` with `BORDER_SUBTLE`.

### Buttons

| Variant | Normal | Hover | Pressed | Text |
|---|---|---|---|---|
| Primary | `BRAND_RED` | `BRAND_RED_HOVER` | `BRAND_RED_PRESSED` | `#000000`, semibold |
| Secondary | `INPUT_BG`, 1 px `BORDER` | `DEEP_RED`, 1 px `BRAND_RED` | `#2A0B05` | `TEXT_PRIMARY` |
| Ghost | transparent | `DEEP_RED` | `#2A0B05` | `TEXT_SECONDARY` |
| Destructive | transparent, 1 px `ERROR` | `#3A1210` | `#541712` | `ERROR` |

Standard height 36 px, horizontal padding 14 px, radius 4 px. Large transaction action: 42 px. Icons 16 px with 8 px gap. This is the modern descendant of the legacy flat rectangular buttons; do not use pill buttons or large radii.

### Tables

- 38 px header, `#140B09`, `TEXT_SECONDARY` 12 px semibold;
- 38 px rows; alternate `PANEL_BG` / `ROW_ALT`;
- 1 px horizontal separators `BORDER_SUBTLE`;
- selected row `DEEP_RED` with a 2 px `BRAND_RED` left rail;
- right-align all amounts and numeric columns; use tabular/monospace numerals;
- truncate addresses/hashes in the cell but show/copy the full value through tooltip/context action;
- sorting state uses a small red caret; filters sit in a toolbar above the table.

## 8. Header and menus

Header contents:

- left: reconstructed triangle + wordmark lockup;
- right: `File`, `Settings`, `Help` text menu buttons, 36 px high;
- File: Encrypt Wallet, Backup Wallet, Sign Message, Verify Message, separator, Exit;
- Settings: Options and Debug Window;
- Help: About Cryptographic Triangles;
- remove the current `Operations` menu because it duplicates the persistent sidebar.

Menus should use the historical menu colors: `MENU_RED`, `MENU_TEXT`, selected `MENU_SELECTED` / `MENU_SELECTED_TEXT`, 6 px radius, 6 px internal padding. Keep menu accent use localized; do not spread `MENU_RED` across ordinary panels.

## 9. Bottom status bar

Left to right:

1. Connection icon (`connect0_16` through `connect4_16`) + “N connections”; choose the actual 0–4 strength asset by count rather than only 0/4.
2. Sync icon (`synced` or `notsynced`) + block status. The center section expands and contains a thin 120–220 px progress bar when syncing; at 100%, show “Synced · block N” and collapse the bar.
3. Staking icon (`staking_on` or `staking_off`) + `Staking on/off`; use the actual live result.
4. Encryption icon (`lock_closed` or `lock_open`) + `Encrypted/Unencrypted`.
5. Compact transport summary `C n · T n · I n` with a tooltip spelling out clearnet/Tor/I2P.
6. Refresh as a 28 px icon-only ghost button with tooltip, not a text button.

Icons are 16 px. Clusters have 12 px horizontal padding and 1 px separators. Success state may tint icons/text `SUCCESS`; syncing `WARNING`; disconnected/error `ERROR`; idle metadata `TEXT_MUTED`. Remove the duplicate “staking off / unencrypted” rows from the sidebar.

## 10. Page-by-page specification

### 10.1 Overview

Header: `Overview` / `Wallet balance and recent activity`.

Top grid at widths ≥900 px: balance card 42%, recent transactions 58%; stack below that breakpoint.

Balance card follows the historical Wallet frame exactly:

- Spendable
- Stake
- Unconfirmed
- Immature
- divider
- Total

Labels are `TEXT_SECONDARY`; amounts are right-aligned tabular values with `TRI`. Total is 22 px semibold; Spendable gets a small red triangle marker. Use tooltips from the historical form. The snapshot/coverage/reconciliation sentence becomes a collapsed `Data details` row, not primary copy.

Recent transactions card: title and `View all` action to Transactions. Rows show status/type icon, time, label or shortened address, and signed amount. Incoming positive amounts use `SUCCESS`; outgoing negative values use `TEXT_PRIMARY`; immature uses `WARNING`. Show 6–8 rows and an intentional empty state.

Move wallet backup into a small `Wallet actions` card below, with a path picker field and secondary `Back up wallet` action. Put the live peers table in a collapsed `Network details` disclosure or link it from the status connection cluster; it must not dominate the default Overview.

### 10.2 Send Coins

Header: `Send Coins` / `Create, review, sign, and broadcast a Triangles transaction`.

Use a visible three-step rail: **1 Details → 2 Review signed transaction → 3 Broadcast**. State, not separate routes, drives the step.

Details card:

- `Send from` full-width funded-address combo; show label/address and right-aligned available balance;
- recipient block with `Pay to`, address-book picker, paste action, `Label`, `Amount`, and optional note where supported;
- `Add recipient` secondary button; preserve the historical multi-recipient concept even if initially disabled with an honest tooltip;
- `Fee` and custom change under an `Advanced` disclosure;
- Coin Control Features as its own collapsible card, default closed unless enabled and previously opened.

Coin control table columns: checkbox, transaction/output, address/label, amount, confirmations/height. Summary strip must preserve the legacy concepts: Quantity, Bytes, Amount, Priority (if available), Fee, Low Output (if determinable), After Fee, Change. Unknown values display `—`, never invented data. `Clear all` is a ghost action.

Primary footer action: `Review & sign` (42 px). Remove the current long engineering banner from the main flow. Replace with a small lock icon + `Signing happens locally. Broadcasting uses your connected tri7d.` and a details tooltip.

Review card shows recipient(s), input count, amount, fee, change, byte size, and txid in a two-column summary. Raw hex is collapsed under `Raw signed transaction`. Actions: secondary `Back`, secondary `Save signed transaction`, primary `Broadcast transaction`. While broadcasting, disable repeat submission and show a spinner. Success is a compact success panel with txid and Copy action; failure is an inline error panel that preserves the signed transaction and allows retry.

### 10.3 Receive Coins

Header: `Receive Coins` / `Manage receiving addresses and create payment requests`.

Two-column layout:

- left 60%: receiving-address table with Label, Address, Source and row selection;
- right 40%: `Payment request` card with selected address, Label, Message, Amount, and `Request a specific amount` switch.

Toolbar above addresses: `New address` primary, Copy Address and Edit Label secondary/ghost. Clearly but quietly tag non-wallet sources (`GUI keystore`) with a neutral badge. Wallet write capability is an info callout only when unavailable; do not show implementation prose in the normal case.

Generate and display the QR **inline** in the payment-request card as soon as the selected address/request fields are valid. Use a crisp nearest-neighbor QR on a mandatory white quiet-zone tile. Below it show the full `Triangles:` URI in a selectable monospace field. Actions: Copy URI, Save QR. The modal may remain for an enlarged preview, not as the only QR view.

### 10.4 Address Book

Header: `Address Book` / `Labels and addresses used by this wallet`.

Toolbar: search field, source filter (`All`, `wallet.dat`, `GUI keystore`, `Address book`), `New address` primary. Table: Label, Address, Source, Actions. Do not edit labels continuously inside every table row; selection or double-click opens an edit dialog, matching the old instruction while avoiding accidental writes. Row context/actions: Copy Address, Edit Label, Sign Message, Verify Message, Delete. Delete uses a confirmation dialog and destructive styling. Keep immutable wallet rows visibly read-only with a tooltip.

### 10.5 Transactions

Header: `Transactions` / `History for this wallet`.

Filter toolbar modeled on the historical page: date range/type combo, status combo, minimum amount, and search across label/address/txid. Table columns: Status icon, Date, Type, Label/Address, Amount, Confirmations. Default newest first. Amounts right-aligned; confirmed icon uses `transaction2.png`, pending uses clock sequence, conflicts use a regenerated conflict icon.

Selecting a row opens a right-side details drawer (or modal below 900 px) with full txid, status, confirmations, date, source/destination, debit/credit, fee, net amount, and raw/detail disclosure. Provide Copy TxID and `Open in Block Explorer`. Empty and loading states occupy the table body, not a loose label.

### 10.6 Staking

Header: `Staking` / `Proof-of-stake status and wallet weight`.

Top status card: large `staking_on/off` icon, `Staking active`, `Ready but not currently staking`, or `Staking off/error`; never claim state from configuration alone. Show Last updated and Refresh.

Metric cards: Wallet weight, Network weight, Expected time, Mature balance/eligible inputs, Reserve. Use `—` for missing RPC fields. Below: eligibility/explanation panel and a compact raw RPC disclosure for diagnostics. Reserve editing belongs in Options; link there. Use `SUCCESS` only when actively staking, `WARNING` when enabled but ineligible, and `TEXT_MUTED` when disabled.

### 10.7 Messages

Header: `Messages` / `Encrypted messages sent and received through Triangles`.

Restore the old master/detail mental model:

- segmented control: Inbox, Sent, Compose; show unread badge on Inbox;
- message list columns: unread marker, Contact, Preview, Date;
- selected message opens a reading pane with From, To, Date/status, full body and Back on narrow layouts;
- actions: Conversation, Reply/Send, Copy From Address, Copy To Address, Delete.

Compose card: From combo, To field with address-book picker, multiline Message, primary Send. Show delivery/RPC errors inline and retain draft text on failure. Raw RPC/local transport data goes behind a `Message diagnostics` disclosure.

### 10.8 Block Explorer

Header: `Block Explorer` / `Inspect blocks, transactions, addresses, and chain statistics`.

Top search field accepts height, block hash, txid, or address; primary `Search`. Below, tabs: Overview, Block, Transaction, Address, Rich List. Preserve all current explorer data and navigation, but use shared cards and tables. Hash/address values are monospace with Copy icons. Previous/next block are compact secondary buttons. PoS is a `BRAND_RED` outlined badge; PoW is neutral. Errors appear immediately under search. Latest blocks table and chain statistics are the default Overview tab.

### 10.9 Options

Header: `Options` / `Wallet and interface preferences`.

Use the historical tab structure: **Main, Network, Window, Display**. Keep unsupported controls present but disabled with a concise `Requires daemon integration` or `Not supported by this desktop shell` help line. Never create local toggles that imply daemon behavior.

- Main: transaction fee, staking reserve, start-on-login, detach databases.
- Network: UPnP, proxy enable/IP/port/version.
- Window: minimize-to-tray, minimize-on-close.
- Display: language, amount unit, display addresses, coin control, onion address in status bar.

Sticky footer: Cancel, Apply, OK. Apply saves without leaving; OK saves and returns to the previous screen, not always Overview. Validation occurs before saving. Existing implemented settings remain functional.

### 10.10 Sign and Verify Message

These are tool pages in the sidebar group. Use the shared page/card form. Address fields include address-book picker and paste. Message is a multiline input. Signature is a multiline monospace selectable field. Primary actions are `Sign message` and `Verify message`; Clear is secondary. Verification success/failure appears as a bordered semantic result panel, never only colored text.

### 10.11 About

Modal width 480 px. At top, centered reconstructed compact transparent logo at about 260 × 64 px. Then:

- `Cryptographic Triangles`
- `tri7 version {CARGO_PKG_VERSION}`
- `Modern client engine with byte-exact legacy-chain replay.`
- copyright lines retained from the historical dialog;
- optional build/commit information in a collapsed `Build details` section;
- one primary `OK` button right-aligned.

Do not use `images/about.png`: it is a 77 × 548 legacy strip and is currently distorted to 200 × 200.

## 11. Asset plan

### Keep as historical references or functional fallbacks

- `icons/connect0_16.png` … `connect4_16.png`
- `icons/clock1.png` … `clock5.png`
- `icons/synced.png`, `notsynced.png`
- `icons/staking_on.png`, `staking_off.png`
- `icons/lock_open.png`, `lock_closed.png`
- `icons/transaction2.png`, `tx_inout.png`, `tx_mined.png`
- all `menu_16/*.png` until clean replacements cover the same semantic inventory;
- `icons/triangles-16.png`, `triangles-32.png`, `triangles-128.png` for OS/window compatibility until regenerated equivalents are verified;
- `images/splash.png` only as a temporary launch/load fallback.

### Replace before final visual sign-off

- `images/header_logo.png`: rebuild as `brand_header@2x.png` with transparency, or SVG if the image loader supports it; never scale the 300 × 63 JPEG-like raster down non-proportionally.
- `images/about.png`: replace with `brand_compact@2x.png`; do not reuse the vertical strip.
- `images/splash.png`: rebuild at 1024 × 576 or remove the splash image in favor of the header lockup plus a native progress/loading state.
- `images/wallet.png`: remove from active UI unless a clear empty-state use is designed.
- navigation icons: redraw as a single coherent 20/24 px set, preserving the old silhouettes and meanings. Preferred format is simple monochrome SVG rasterized at 1×/2× or crisp transparent PNGs. Default tint `TEXT_MUTED`, active tint `BRAND_RED`/`TEXT_PRIMARY`.
- status and transaction icons: redraw at 16 and 32 px with the historical shapes but cleaner edges and transparency. Preserve state filenames or update the asset map atomically.

### New required assets

- clean triangle mark at 16, 20, 32, 64, 128, 256 px;
- transparent header and compact lockups at 1× and 2×;
- staking navigation icon;
- conflict/error transaction icon;
- optional small Copy, Refresh, Chevron, Search, QR, External/Open, and More icons matching the new line weight.

All new raster assets must be PNG with alpha, authored at 2×, downsampled with a high-quality filter, and visually checked at the exact render size. Do not derive production assets by JPEG background removal. Keep the supplied brand JPEGs unchanged in `brand/` as references.

## 12. Concrete egui/eframe implementation notes

### 12.1 Theme constants and `Visuals`

Replace the current global red override. Equivalent intent:

```rust
let mut visuals = egui::Visuals::dark();
visuals.override_text_color = Some(Color32::from_rgb(0xF2, 0xE9, 0xE6));
visuals.panel_fill = Color32::from_rgb(0x00, 0x00, 0x00);
visuals.window_fill = Color32::from_rgb(0x12, 0x12, 0x12);
visuals.extreme_bg_color = Color32::from_rgb(0x1C, 0x1C, 0x1C);
visuals.faint_bg_color = Color32::from_rgb(0x12, 0x12, 0x12);
visuals.code_bg_color = Color32::from_rgb(0x0D, 0x0D, 0x0D);
visuals.selection.bg_fill = Color32::from_rgb(0x3D, 0x0E, 0x04);
visuals.selection.stroke = egui::Stroke::new(1.0, Color32::from_rgb(0xE3, 0x21, 0x05));
visuals.hyperlink_color = Color32::from_rgb(0xF0, 0x5A, 0x38);
visuals.window_stroke = egui::Stroke::new(1.0, Color32::from_rgb(0x5B, 0x20, 0x13));
visuals.window_shadow = egui::Shadow::NONE;
```

Widget visuals:

- `noninteractive`: `PANEL_BG`, `BORDER_SUBTLE`, `TEXT_SECONDARY`;
- `inactive`: `INPUT_BG`, `BORDER`, `TEXT_PRIMARY`;
- `hovered`: `DEEP_RED`, `BRAND_RED`, `TEXT_PRIMARY`;
- `active`: `#2A0B05`, `BRAND_RED`, `TEXT_PRIMARY`;
- `open`: same as hovered;
- `rounding`: 4 px controls, 6 px cards, 8 px windows; avoid blanket rounding above 8 px;
- `expansion`: 0–1 px only so rows do not jump visibly.

Primary and destructive buttons require local constructors (`primary_button`, `secondary_button`, etc.); global widget visuals cannot represent all variants correctly.

### 12.2 Spacing and sizing

Configure:

```rust
style.spacing.item_spacing = egui::vec2(8.0, 8.0);
style.spacing.button_padding = egui::vec2(14.0, 8.0);
style.spacing.menu_margin = egui::Margin::same(6.0);
style.spacing.indent = 18.0;
style.spacing.interact_size = egui::vec2(36.0, 36.0);
style.spacing.slider_width = 160.0;
style.spacing.combo_width = 220.0;
style.spacing.text_edit_width = 280.0;
```

Use 4/8/12/16/24/32 px as the spacing scale. Page padding 24; card padding 16; form row gap 12; label-to-control gap 8. Avoid hard-coded 22 px text edits from the current code.

### 12.3 Reusable view helpers

Introduce view-only helpers in `app.rs` (or a later `gui/theme.rs` / `gui/widgets.rs`) for:

- `page_header(ui, title, subtitle)`;
- `card(ui, title, body)`;
- `primary_button`, `secondary_button`, `ghost_button`, `danger_button`;
- `nav_row(screen, icon, label, badge, selected)` with one combined response/hit target;
- `status_cluster(icon, label, semantic_state)`;
- `copyable_monospace(value)`;
- `empty_state(icon, title, body, optional_action)`;
- `inline_notice(kind, title, body)`;
- `table_header` and consistent row cells.

Use `Frame::new()`/`Frame::none()` with explicit fill, stroke, inner margin, and rounding. Scope visual changes with `ui.scope()` so primary-button colors do not leak into adjacent controls.

### 12.4 Layout and responsiveness

- Keep the sidebar fixed at 216 px, status/header fixed heights, central content scrollable.
- Use `ui.available_width()` breakpoints around 900 px to switch two-column pages to stacked layouts.
- Give tables horizontal scrolling only when columns cannot sensibly compress; keep primary text columns flexible.
- Do not nest unrestricted vertical `ScrollArea`s. A page owns vertical scrolling; inner lists get explicit maximum heights only where master/detail behavior requires it.
- Preserve keyboard focus order from top-left to bottom-right. All icon-only buttons require tooltips and accessible labels.

### 12.5 Texture handling

- Load UI art with `TextureOptions::LINEAR`; render QR with `TextureOptions::NEAREST`.
- Always preserve source aspect ratio. Eliminate the current `about.png` 77×548 → 200×200 distortion and header 300×63 → 150×49 distortion.
- Cache static textures as now. For dynamic QR, cache by URI/hash rather than recreating a texture every frame.
- If icon tinting is needed, prefer white/alpha mask assets and `egui::Image::tint` over maintaining multiple colored rasters.

### 12.6 Behavioral constraints

- This is a view redesign. Do not weaken existing signing, wallet-write, RPC, broadcast, or explorer semantics.
- All unavailable values display `—`; all unsupported controls explicitly say why.
- Destructive or irreversible actions require confirmation. Broadcast requires the explicit Review step.
- Busy operations disable their initiating action and prevent duplicate submission.
- Surface concise user-facing language first; preserve detailed RPC/engine errors in expandable details or copyable diagnostics.

## 13. Definition of done

The redesign is complete only when:

- the window is immediately recognizable as Cryptographic Triangles without using a pixelated legacy bitmap;
- the original six-page primary order and labels remain intact;
- every current tri7 page/function remains reachable;
- default body text is neutral, red is purposeful, and all states remain readable;
- header, sidebar, cards, forms, tables, dialogs, and status bar share one spacing/state system;
- Send visibly follows Details → Review/sign → Broadcast;
- Receive shows an inline crisp QR and payment URI;
- Overview preserves Spendable/Stake/Unconfirmed/Immature/Total and recent transactions;
- status icons reflect actual connection, sync, staking, and encryption state;
- no image is visibly stretched, blurred, JPEG-boxed, or incorrectly aspect-ratio scaled;
- the UI remains usable at 1040 × 700 and scales cleanly upward.
