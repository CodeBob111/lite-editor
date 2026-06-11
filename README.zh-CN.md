# Nib

[English](./README.md) | **简体中文**

纯 Rust 编写的原生 macOS 代码编辑器，基于 [gpui](https://github.com/zed-industries/zed/tree/main/crates/gpui)（Zed 的 GPU 加速 UI 框架）与 [gpui-component](https://github.com/longbridge/gpui-component)。无 webview、无 JavaScript——整个应用是一个原生二进制。

> 2026-06：原 Tauri 2 + CodeMirror 6 实现已全量重写为原生 Rust（绞杀者迁移，M0–M5）。切换完成后旧 `src/` + `src-tauri/` 已删除。

## 功能

- 代码编辑：tree-sitter 语法高亮（Warm Earth 主题）、折叠、软换行
- 项目文件树、模糊快速打开（双击 Shift / Cmd+P）、全项目搜索（Cmd+Shift+F）
- Git：变更 / 提交 / 推拉 / 分支 / 历史，自绘 diff 视图，3-way merge 冲突解决
- Java LSP（jdtls）：诊断、跳转定义（F12）、查找引用（Shift+F12）
- Maven 依赖树 + 冲突高亮
- 内置终端（alacritty_terminal 内核，ctrl-`）
- Markdown 预览（Cmd+Shift+V）、Arthas 命令生成、Astore 集成
- 会话恢复、最近项目、编辑器设置（Cmd+,）

## 架构

```
crates/
├── nib-core/   # UI 无关内核:fs/搜索/git/maven/lsp/终端/diff/…
│               # 自持 tokio runtime;主线程永不做阻塞 IO
└── nib-app/    # gpui 应用:工作台、面板、浮层、主题
```

性能纪律：所有 IO 与子进程都在 core runtime 上跑；异步回写带陈旧守卫；帧时哨兵记录主线程 >32ms 的卡顿。高频源（PTY 输出）走脏标记拉模型而非事件推送。

## 构建与安装

```bash
cargo build --release -p nib-app   # release 二进制
scripts/bundle-nib.sh              # → target/release/bundle/Nib.app(ad-hoc 签名)
cp -R target/release/bundle/Nib.app /Applications/
```

测试与检查：

```bash
cargo test -p nib-core
cargo clippy --workspace --all-targets
```

## 许可

私人项目。
