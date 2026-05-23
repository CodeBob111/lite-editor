# ADR-001: 编辑器技术栈选型

**日期**: 2026-05-23
**状态**: 已决定
**决定**: Tauri + CodeMirror 6

## 背景

从 IntelliJ IDEA 迁移到自研轻量编辑器，核心需求：
1. **Find Usages** — 方法/类跨文件引用查找（语义级，非文本搜索）
2. **Scroll to Source** — 当前文件定位到目录树
3. **Maven 管理面板** — 模块树展示、命令执行、输出流

目标语言：Java、Python、TypeScript
性能目标：编辑器壳 <100MB 内存，冷启动 <2s

## 候选方案

### 方案 A: Tauri + Monaco Editor

**架构**: Rust 后端 + WebView (Monaco)

| 维度 | 评估 |
|------|------|
| 内存 | 80-120MB（空项目），150-200MB（大项目，不含 LSP） |
| 冷启动 | 1-2s |
| LSP 集成 | `monaco-languageclient` 成熟，开箱即用 |
| Find Usages | Monaco 内置 peek references 视图 + 可自定义面板 |
| Maven 面板 | 自由实现，HTML/CSS 树组件生态丰富 |
| 大文件 | 流畅（内置虚拟化） |

**优势**:
- VS Code 同款编辑器核心，十年打磨
- LSP 集成省约 1 周开发量
- UI 体验最接近 IntelliJ

**风险**:
- Monaco 官方只测 Chromium，macOS WKWebView 有已知 selection/IME/scroll 问题
- Bundle 较大 (~2MB min)
- 定制灵活度不如 CodeMirror

**开发周期**: MVP 约 2-3 周

### 方案 B: Tauri + CodeMirror 6 ✅ 选定

**架构**: Rust 后端 + WebView (CodeMirror 6)

| 维度 | 评估 |
|------|------|
| 内存 | 50-80MB（空项目），80-120MB（大项目，不含 LSP） |
| 冷启动 | 0.8-1.5s |
| LSP 集成 | 无成熟库，需自己写 adapter (~500-800 行) |
| Find Usages | 需自己实现面板，CM6 Panel API 灵活 |
| Maven 面板 | 同方案 A |
| 大文件 | 流畅（内置虚拟化） |

**优势**:
- Bundle 小 (~150KB min+gzip)，内存比 Monaco 低约 40%
- 纯函数式架构，扩展性极好
- 明确支持 Safari/WebKit（Tauri macOS 用 WKWebView），无兼容风险
- 长期可维护性更好

**劣势**:
- LSP 集成需自己写 adapter，多花约 1 周
- 社区 LSP 桥接库成熟度一般

**开发周期**: MVP 约 3-4 周

### 方案 C: 原生 Swift (macOS)

**架构**: SwiftUI/AppKit + NSTextView/STTextView

| 维度 | 评估 |
|------|------|
| 内存 | 30-50MB |
| 冷启动 | 0.3-0.5s |
| LSP 集成 | 需自己写全套 JSON-RPC over stdio (~1000-1500 行) |
| Find Usages | 完全自由但全部手写 |
| Maven 面板 | NSOutlineView / SwiftUI DisclosureGroup |
| 大文件 | 需自己实现虚拟化，否则卡顿 |

**优势**:
- 最轻量，macOS 原生体验
- 无 Web 层开销

**劣势**:
- 仅 macOS
- 代码编辑器文本组件工作量巨大（多光标、undo/redo、IME、语法高亮虚拟化）
- 几乎没有 Swift 代码编辑器方向的社区资源
- 烂尾概率高

**开发周期**: MVP 约 5-7 周

### 已排除: Zed 扩展

扩展 API 不支持自定义 side panel，无法实现 Maven 管理面板。Find Usages UI 不可定制。

## 决定

选择 **方案 B: Tauri + CodeMirror 6**。

**理由**:
1. 内存目标 <100MB 最有把握
2. WebKit 兼容性无风险（CM6 官方支持 Safari）
3. 架构更干净，长期扩展性好
4. 多花的 LSP 集成工作量可接受（1 周）
5. 技术栈匹配（TypeScript 前端熟悉）

## LSP 服务进程资源（不可控，各方案相同）

| 语言 | LSP 服务 | 内存 |
|------|---------|------|
| Java | Eclipse JDT LS | 300MB-1GB |
| Python | Pyright | 100-300MB |
| TypeScript | ts-server | 100-200MB |
