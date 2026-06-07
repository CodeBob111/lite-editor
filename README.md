# Nib

**English** | [简体中文](./README.zh-CN.md)

A lightweight, native desktop code editor built with [Tauri 2](https://tauri.app/) and [CodeMirror 6](https://codemirror.net/). It bundles a fast multi-tab editor, a full Git client, Java language intelligence, Maven tooling, and an integrated terminal into a single small binary — without the memory footprint of an Electron app.

> Status: early development (`v0.1.0`). Built primarily for Java/Maven workflows, with first-class Markdown, Python, and JavaScript support.

---

## Features

### Editor core
- **CodeMirror 6** editing with syntax highlighting for Java, Python, JavaScript, and Markdown.
- Multi-tab editing backed by an **LRU view cache** (keeps up to 30 live editor instances; evicted views are reconstructed on demand).
- Code folding, bracket matching, autocompletion, line wrapping, and find/replace.
- **1-second debounced autosave** — no explicit save needed, though `Cmd/Ctrl+S` is available.
- **Reload-on-external-change**: a native filesystem watcher detects when an open file is modified on disk by another tool (e.g. `git checkout`, a formatter, another editor) and reloads it automatically. Unsaved local edits are never clobbered — you get a non-destructive warning instead.
- **Session restore**: reopens your projects, files, and active tab on the next launch.

### Workbench & UI
- **Dark, single-accent design system** — a cobalt-accent dark theme driven by a centralized token palette (backgrounds, text, borders, syntax colors) shared by the UI and the CodeMirror theme.
- **Activity bar** on the left switches the sidebar between **Explorer**, **Commit** (working-tree changes), **Git** (branches + log), and **Maven** (modules + build output), with a **Settings** gear pinned at the bottom.
- **Settings / preferences UI** (`Cmd/Ctrl` + `,`): editor font family/size, tab size, word wrap, bracket matching, and code folding — bound to a real `settings.json` (persisted to the app-data dir, with a raw-JSON editing tab) and applied live to the editor.
- **Welcome / start screen** with quick actions (open folder, clone, new terminal) and a recent-projects list, shown when no file is open.
- **Status bar** with the current branch, diagnostic counts, cursor position, indentation, encoding, line ending, and language.
- **Breadcrumb** of the active file's path under the tab bar.
- **IDEA-style find toolbar** (`Cmd/Ctrl` + `F`) with a live match count and case / whole-word / regex toggles.
- **Copy from anywhere selectable** — Markdown preview, Git panel, file tree, status bar, and other read-only surfaces, not just the editor.

### Language intelligence
- **LSP integration** (Java): go-to-definition (`F12` or `Cmd`+click), find references, and inline diagnostics rendered in the lint gutter.
- **Java class index**: a Rust-side symbol index over your project and its Maven dependencies for fast class lookup and navigation.

### Git client
A near-complete Git UI driven by native `git` commands:
- Branch management — create, checkout, rename, delete, prune.
- `pull` / `fetch` / `push`, `commit` with patch-level (hunk) staging, and `log`.
- **Inline blame** in the editor gutter.
- **Merge with a conflict-resolution UI** — view each side, pick a version, or resolve per file; plus `rebase`, `cherry-pick`, and `revert`.
- `clone` from a remote, and automatic **multi-repository discovery** across an open workspace.
- A side-by-side **diff viewer** and a working-tree **changes panel**.

### Build & tooling
- **Maven**: parses modules from `pom.xml`, runs Maven goals, and streams output back to the UI.
- **Integrated terminal**: real PTY sessions via [`portable-pty`](https://crates.io/crates/portable-pty) rendered with [xterm.js](https://xtermjs.org/) — multiple terminals, resizing, and Claude CLI status detection.
- **Markdown live preview** powered by [`marked`](https://marked.js.org/).
- **Arthas helper**: generates [Arthas](https://github.com/alibaba/arthas) diagnostic commands (watch/trace/jad, etc.) from the Java symbol under the cursor.

### Workspace
- Multiple project roots, a recent-projects list, a file tree with full CRUD (create / rename / copy / delete), fuzzy **quick-open**, and **find-in-files**.
- A built-in **performance monitor** with an exportable report.

---

## Tech stack

| Layer | Technologies |
| --- | --- |
| Frontend | TypeScript, Vite 6, CodeMirror 6, xterm.js, marked |
| Backend | Rust, Tauri 2, tokio, `notify` (filesystem watch), `portable-pty` (terminal), `walkdir`, `rayon`, `quick-xml` (pom parsing), `reqwest` (HTTP) |
| Plugins | `tauri-plugin-shell`, `tauri-plugin-dialog` |

---

## Project structure

```
lite-editor/
├── index.html              # App shell
├── src/                    # Frontend (TypeScript)
│   ├── main.ts             # Entry point, menu wiring, file-watcher reload
│   ├── editor-setup.ts     # CodeMirror state, autosave, external-reload
│   ├── tabs.ts             # Tab manager
│   ├── state.ts            # Global app state + LRU editor cache
│   ├── git-panel.ts        # Git UI (left sidebar view)
│   ├── lsp-*.ts            # LSP client & navigation
│   ├── maven-helper.ts     # Maven panel (left sidebar view)
│   ├── terminal-panel.ts   # Integrated terminal
│   ├── md-preview.ts       # Markdown preview
│   ├── settings.ts         # Preferences state, persistence, live-apply
│   ├── settings-ui.ts      # Settings screen (Cmd+,)
│   ├── status-bar.ts       # Status bar (branch/diagnostics/cursor/lang) + breadcrumb
│   ├── welcome-screen.ts   # Welcome / start screen
│   └── ...
├── src-tauri/              # Backend (Rust)
│   ├── src/
│   │   ├── lib.rs          # Tauri builder, native menu, command registry
│   │   ├── commands.rs     # File ops, search, Maven, file watcher, session, settings
│   │   ├── git.rs          # Git command surface
│   │   ├── lsp.rs          # Language Server Protocol bridge
│   │   ├── java_index.rs   # Java class indexer
│   │   └── terminal.rs     # PTY management
│   └── tests/              # Rust integration tests
└── package.json
```

---

## Getting started

### Prerequisites
- [Node.js](https://nodejs.org/) 18+
- [Rust](https://rustup.rs/) (stable toolchain)
- Platform prerequisites for Tauri 2 — see the [Tauri prerequisites guide](https://tauri.app/start/prerequisites/).

### Install
```bash
npm install
```

### Run in development
```bash
npm run tauri dev
```

### Build a native bundle
```bash
npm run tauri build
```

The frontend alone can be type-checked and built with `npm run build` (`tsc && vite build`). Rust tests run from `src-tauri/`:
```bash
cd src-tauri && cargo test
```

---

## Keyboard shortcuts

| Action | Shortcut |
| --- | --- |
| Open folder | `Cmd/Ctrl` + `O` |
| Save | `Cmd/Ctrl` + `S` |
| Close tab | `Cmd/Ctrl` + `W` |
| Find in current file | `Cmd/Ctrl` + `F` |
| Find in files | `Cmd/Ctrl` + `Shift` + `F` |
| Quick open file | double-tap `Left Shift` |
| Settings / preferences | `Cmd/Ctrl` + `,` |
| Go to definition | `F12` or `Cmd` + click |
| Navigate back / forward | `Cmd/Ctrl` + `[` / `]` |
| Export performance report | `Cmd/Ctrl` + `Shift` + `P` |

Explorer, Commit, Git, and Maven are switched from the left **activity bar**; the integrated terminal is toggled from the **View** menu.

---

## License

No open-source license is currently specified; the project is marked `private`. Treat it as all-rights-reserved unless a `LICENSE` file is added.
