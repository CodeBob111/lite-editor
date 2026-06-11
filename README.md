# Nib

**English** | [简体中文](./README.zh-CN.md)

A fast, native macOS code editor written entirely in Rust on [gpui](https://github.com/zed-industries/zed/tree/main/crates/gpui) (Zed's GPU-accelerated UI framework) and [gpui-component](https://github.com/longbridge/gpui-component). No webview, no JavaScript — the entire app is a single native binary.

> 2026-06: the original Tauri 2 + CodeMirror 6 implementation was fully rewritten in native Rust (strangler migration, M0–M5). The old `src/` + `src-tauri/` tree was removed after switchover.

## Features

- Code editing with tree-sitter syntax highlighting (Warm Earth theme), folding, soft-wrap
- Project file tree, fuzzy quick-open (double-Shift / Cmd+P), project-wide search (Cmd+Shift+F)
- Git: changes / commit / push / pull / branches / log, self-drawn diff view, 3-way merge conflict resolution
- Java LSP (jdtls): diagnostics, go-to-definition (F12), find usages (Shift+F12)
- Maven dependency tree with conflict highlighting
- Integrated terminal (alacritty_terminal backend, ctrl-`)
- Markdown preview (Cmd+Shift+V), Arthas command generation, Astore integration
- Session restore, recent projects, editor settings (Cmd+,)

## Architecture

```
crates/
├── nib-core/   # UI-agnostic kernel: fs/search/git/maven/lsp/terminal/diff/…
│               # owns its tokio runtime; main thread never does blocking IO
└── nib-app/    # gpui application: workbench, panels, overlays, theme
```

Performance discipline: all IO and subprocess work runs on the core runtime; async write-backs carry stale guards; a stall sentinel logs main-thread frames over 32ms. High-frequency sources (PTY output) use a dirty-pull model instead of event push.

## Build & install

```bash
cargo build --release -p nib-app   # release binary
scripts/bundle-nib.sh              # → target/release/bundle/Nib.app (ad-hoc signed)
cp -R target/release/bundle/Nib.app /Applications/
```

Tests and lints:

```bash
cargo test -p nib-core
cargo clippy --workspace --all-targets
```

## License

Private project.
