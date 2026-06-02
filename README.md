# Lite Editor

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

> **Internal integration — Astore.** The `astore` module integrates with an Alibaba-internal code/asset hosting platform (SSO login, project detection, code sync, and publish). It only functions inside that corporate environment and is inert elsewhere.

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
│   ├── git-panel.ts        # Git UI
│   ├── lsp-*.ts            # LSP client & navigation
│   ├── maven-helper.ts     # Maven panel
│   ├── terminal-panel.ts   # Integrated terminal
│   ├── md-preview.ts       # Markdown preview
│   └── ...
├── src-tauri/              # Backend (Rust)
│   ├── src/
│   │   ├── lib.rs          # Tauri builder, native menu, command registry
│   │   ├── commands.rs     # File ops, search, Maven, file watcher, session
│   │   ├── git.rs          # Git command surface
│   │   ├── lsp.rs          # Language Server Protocol bridge
│   │   ├── java_index.rs   # Java class indexer
│   │   ├── terminal.rs     # PTY management
│   │   └── astore.rs       # Internal platform integration
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
| Find in files | `Cmd/Ctrl` + `Shift` + `F` |
| Quick open file | double-tap `Left Shift` |
| Go to definition | `F12` or `Cmd` + click |
| Navigate back / forward | `Cmd/Ctrl` + `[` / `]` |
| Export performance report | `Cmd/Ctrl` + `Shift` + `P` |

Terminal, Git, and Astore panels are toggled from the **View** menu.

---

## License

No open-source license is currently specified; the project is marked `private`. Treat it as all-rights-reserved unless a `LICENSE` file is added.
