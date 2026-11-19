# CodeBot desktop interface

The desktop app is a local development assistant with CLI parity. Processing can use local rule-based diagnostics, a user-specified GGUF or safetensors model served by an owned llama.cpp or Transformers process, or configured AI APIs. Conversation history is opt-in, scoped to one chat and destination, and requires renewed consent after changing the destination or restoring saved chats. Optional local archives preserve chats and drafts. File attachments are explicit, previewed snapshots of selected lines in one workspace. No example prompts or invented integrations belong in the UI.

## Direction

A focused conversation workspace replaces the earlier segmented form. The desktop app belongs beside an editor and follows Material Design 3 (light scheme built around the #005FB8 brand primary): a tonal navigation drawer, a white reading pane, pill-shaped controls, and state layers instead of outlines. Keep restrained sans-serif typography, a readable message measure, and one compact composer. Avoid promotional headings, repeated cards, bright decorative accents, and full-width form rows. Empty and populated conversations have different compositions.

## Shared tokens

Colors are Material 3 color roles.

| Role | Material role | Value |
|---|---|---|
| Shell/sidebar | surface-container | #ECEEF4 |
| Content pane | surface-container-lowest | #FFFFFF |
| Composer, filled text fields | surface-container-high | #E6E8EE |
| Divider | outline-variant | #C3C7CF |
| Unselected selection control | outline | #73777F |
| Primary text | on-surface | #191C20 |
| Secondary text | on-surface-variant | #43474E |
| Primary action | primary, white icon | #005FB8 |
| Primary action hover | primary + 8% on-primary | #146CBE |
| User message, new-chat FAB | primary-container / on-primary-container | #D1E4FF / #001D36 |
| Selected row, open page, tonal button | secondary-container / on-secondary-container | #D7E3F7 / #101C2B |
| Disabled container / content | on-surface 12% / 38% | #E1E2E6 / #A0A2A6 |
| Error | error | #BA1A1A |
| Hover / press state layer | content color overlay | 8% / 12% |
| Focus indicator | primary, 2px, outside the control | #005FB8 |
| Shape | full (buttons, drawer items, search), large (pane, FAB) 16px, extra-large (composer) 28px | |
| Elevation | FAB level 1 (level 2 on hover), menus level 2 | |
| Maximum message width | | 680 logical pixels |
| Maximum composer width | | 740 logical pixels |
| Composer padding | | 18px horizontal, 16px vertical |
| Text field padding | | 14px horizontal, 8px vertical |
| Control height | | 36 logical pixels (drawer items 44, FAB 52) |
| Sidebar width | | 224 logical pixels |
| Main text | body | LINE Seed JP Regular 400 (default), 15 logical pixels |
| Conversation line height | | 22px input, 25px response |
| Labels | label large / medium | 14 / 12 logical pixels |
| Page heading | headline small | 22 logical pixels |
| Empty title | headline medium | 28 logical pixels |

These tokens live in `crates/codebot-gui/src/main.rs`. The UI has no decorative motion. The only startup motion is the loading state below, which reports real work. Keep native egui keyboard and focus behavior for controls; authored line icons are drawn inside actual buttons with accessible names.

## Layout and behavior

- The new-chat action is an extended FAB (primary container, drawn plus icon before the label). Below it, a full-radius search bar with a drawn search icon. The sidebar is a left-aligned session list of pill-shaped drawer items, with settings at the bottom. Keep titles on one line and ellipsize to the available width. Selection uses the secondary-container indicator, not an extra badge or colored rule.
- A quiet top app bar contains session context, page destinations as text buttons (tonal while their page is open; 実行 opens a menu), and new-chat/settings icon buttons. Icon buttons are circular with a state layer. Omit a global status bar and result counts.
- An empty conversation places a short heading above the composer in the central reading area. A populated conversation anchors the composer near the bottom and scrolls history independently.
- User messages use a narrow right-aligned primary-container bubble; responses render Markdown on the conversation background. Support headings, lists, tables, quotes, links, inline code, and highlighted monospace code blocks with block-level copy. Keep wide tables inside a horizontal scroll area so they cannot widen the shell. HTML is displayed as text; do not install image or network loaders. Copy feedback appears beside the copy action.
- Use the 28px extra-large shape for the composer frame, filled with surface-container-high and no outline. The composer text area is exactly two UI font rows high. Place send on its right, not on a separate row. Long logs scroll internally; the frame must not grow with input. Keep the disclosure and keyboard hint outside the frame below it.
- Store drafts per session. Switching chats must not lose unfinished input. Async results always return to their original session.
- Settings use the same reading column, not a floating utility window. Changes apply at the next send; Escape returns to the chat.
- Below 900px, replace the sidebar with a session menu. Support the minimum 700×540 viewport without shrinking text.
- Below the composer, name the active API destination or state that local processing sends nothing externally. Never show a non-networked claim for API mode. Do not repeat it across header and sidebar.

## Chat tools, search and suggestions

- With an API destination, the composer row offers ツールを使う per chat. Sending then runs the tool engine inline below the messages in an outlined card (title, end action, replies, the existing command/change/connector review UI, result confirmation). The final answer is added as a normal assistant message.
- Above the composer, files mentioned in the draft appear as chips (green dot, no close mark) that open the Files page around that line. Attached skills and connectors use chips with a close mark.
- At 80% of the context budget, a one-line notice appears below the composer, with 古い履歴を外す when history is included.
- Answers with a change proposal show a collapsible inline diff (diff colors from the theme).

## Startup loading state

- Saved chats are read on a background thread at startup. Until they are applied, saving, sending and the chat UI stay blocked, so the archive is never overwritten by the empty startup session.
- For the first 300 ms only the shell's surfaces are drawn (shell, drawer, white pane), with no indicator: a fast load shows no loading state at all.
- After 300 ms, draw a skeleton of the chat layout (brand, new-chat FAB, search bar, three chat rows, header title, empty-state title and subtitle, composer) in #DADEE6 blocks that breathe between 55% and 100% every 0.9 s, a 3px primary band moving along the pane's top edge every 1.1 s (inside the 16px corners), and the text 保存したチャットを読み込んでいます… for assistive technology.
- When loading finished after the indicator appeared, the content cross-fades in over 140 ms (cubic out). Repaints are requested only while loading or fading.
- Settings → 表示 → 動きを減らす stops the pulse, replaces the moving band with a static secondary-container band and skips the cross-fade. It is stored in `gui-preferences.toml`.
- Fonts are compiled into the binary and are not part of the loading state.

## Library, skills, connectors and context

- The header has a ライブラリ destination. The library page lists skills (from the app library, the workspace `.claude/skills`, and `~/.claude/skills`) and registered connectors as outlined cards, each with a このチャットで使う selection control. Import and registration forms sit below their lists; deletion needs a second confirming press.
- The composer has a circular add button on the left of the input. It opens a menu of library skills and connectors with selection controls and a link to the library page. Attached items appear above the composer as input chips (colored dot, kind, name, drawn close mark); pressing a chip removes it.
- The composer footer shows a compact context meter: percentage plus a 72px segmented bar. Hovering it shows the breakdown: a full-width segmented bar, one legend row per category (skills, history, attachments, input; tool definitions and consultation data in AIに作業を相談) and free space. Categories that will not be sent are gray with a short reason. Values are characters against `max_context_chars`, labelled as an estimate, never as tokens.
- Segment colors: skills #7D5260, tool definitions #5B5F97, history #005FB8, attachments #2E7D5B, input #B26A00, free space surface-container-high.
- Connector tool requests in AIに作業を相談 show connector, tool, description, arguments, program, working folder, environment variable names and timeout before an explicit run action, then reuse the existing result-confirmation step.

## Validation scope

Linux virtual-display captures cover 1180×800 and 700×540, empty and populated history, settings, and multiline input. Tests cover core/CLI behavior and GUI session state. Windows/macOS runtime and assistive technology remain unverified.

Use LINE Seed JP Regular 400 for all UI text by default, including emphasis labels. Settings lets the user choose BIZ UDPGothic (Regular body, actual Bold section labels). Do not simulate weights. Apply the choice immediately and persist it in `gui-preferences.toml` beside the OS-standard application config file; this file is independent of the diagnostic TOML path. Report read/write errors in Settings. Keep Noto assets as historical material, not active UI fonts. Each active font has a bundled OFL license under `assets/fonts/`.

The default theme is explicitly Light, independent of the OS preference. Settings → 表示 offers ライト（標準）, ダーク and OSに合わせる (follows the OS while running); the choice is stored in `gui-preferences.toml`. Both schemes are Material 3 roles in `crates/codebot-gui/src/theme.rs`, read through accessors such as `ACCENT()`. Dark values: shell #1D2024, pane #111418, raised #282A2F, outline-variant #43474E, on-surface #E1E2E9, on-surface-variant #C3C6CF, primary #A4C9FF with on-primary #00315D, primary-container #004786 / #D4E3FF, secondary-container #3B4858 / #D7E3F8, error #FFB4AB. Text keeps at least 7:1 (secondary text 4.5:1) on the pane and drawer in both schemes; a test checks this. Code blocks use InspiredGitHub (light) and base16-ocean.dark (dark). Use a circular primary background with a white arrow for enabled send; disabled send uses the on-surface 12% container with a 38% arrow. Keep dark text on the light chat surface, and blue focus indicators.

Application checkboxes and radio buttons display a 2px outline ring when unselected, and a primary ring with a filled primary center dot when selected, with a circular state layer on hover/focus. Draw the indicator with vector circles so it stays consistent across fonts and display scales. Use the shared `checkbox::Checkbox` and `checkbox::Radio` widgets for selection and confirmation controls, preserving egui's label click, keyboard, focus, disabled state, and accessibility semantics. Use `text_field`/`text_area` for text inputs so they share Material padding.

API settings live at the top of Settings: service, model ID, base URL, protocol for custom endpoints, masked in-memory API key, environment variable name, output limit and timeout. Persist only non-secret API profiles. Clear the in-memory key when changing provider or endpoint. Show save errors beside the save action.

Local AI has a separate configuration panel for a model path or HF file URL, verification/download progress, llama-server path, load/stop controls, and advanced runtime settings. Model-name presets without verified sources must be visibly marked unverified and must never invent a repository URL. Downloads are explicit; inference sends data only to the owned loopback server. Keep the existing no-example-prompts rule.

Local AI lets the user select llama.cpp or Transformers. A Transformers source is a model folder or HF repository ID/URL, never a single GGUF URL. Show the Python interpreter and device selection for this mode. Preserve the existing llama.cpp settings for backward compatibility.
