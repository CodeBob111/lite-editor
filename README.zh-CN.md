# Lite Editor

[English](./README.md) | **简体中文**

一个基于 [Tauri 2](https://tauri.app/) 和 [CodeMirror 6](https://codemirror.net/) 构建的轻量级原生桌面代码编辑器。它把一个快速的多标签编辑器、完整的 Git 客户端、Java 语言智能、Maven 工具链和集成终端打包进一个体积很小的原生二进制——没有 Electron 应用的内存开销。

> 状态：早期开发阶段（`v0.1.0`）。主要面向 Java/Maven 工作流构建，同时一等支持 Markdown、Python 和 JavaScript。

---

## 功能

### 编辑器核心
- 基于 **CodeMirror 6**，对 Java、Python、JavaScript、Markdown 提供语法高亮。
- 多标签编辑，底层由 **LRU 视图缓存** 支撑（最多保留 30 个活跃编辑器实例，被淘汰的视图按需重建）。
- 代码折叠、括号匹配、自动补全、自动换行、查找/替换。
- **1 秒防抖自动保存**——无需手动保存，同时也保留 `Cmd/Ctrl+S`。
- **磁盘外部改动自动重载**：原生文件系统监听器会检测到打开的文件被其他工具在磁盘上修改（如 `git checkout`、格式化工具、另一个编辑器），并自动重新加载。未保存的本地编辑永远不会被覆盖——这种情况下只会给出一条不破坏内容的提示。
- **会话恢复**：下次启动时重新打开你的项目、文件和当前激活的标签。

### 语言智能
- **LSP 集成**（Java）：跳转到定义（`F12` 或 `Cmd`+点击）、查找引用、在 lint 槽（gutter）中渲染内联诊断。
- **Java 类索引**：在 Rust 侧对项目及其 Maven 依赖建立符号索引，用于快速的类查找与导航。

### Git 客户端
一套由原生 `git` 命令驱动、近乎完整的 Git 界面：
- 分支管理——创建、切换、重命名、删除、清理（prune）。
- `pull` / `fetch` / `push`、支持 patch 级（按 hunk）暂存的 `commit`、以及 `log`。
- 编辑器槽内的**内联 blame**。
- **带冲突解决界面的合并**——查看每一方、选择某个版本，或按文件解决；另含 `rebase`、`cherry-pick`、`revert`。
- 从远程 `clone`，以及在打开的工作区中**自动发现多个仓库**。
- 并排**差异查看器**和工作区**改动面板**。

### 构建与工具
- **Maven**：从 `pom.xml` 解析模块、运行 Maven 目标（goal）、把输出流式回传到界面。
- **集成终端**：通过 [`portable-pty`](https://crates.io/crates/portable-pty) 提供真实 PTY 会话，用 [xterm.js](https://xtermjs.org/) 渲染——支持多个终端、尺寸调整、以及 Claude CLI 状态检测。
- **Markdown 实时预览**，由 [`marked`](https://marked.js.org/) 驱动。
- **Arthas 辅助**：根据光标所在的 Java 符号，生成 [Arthas](https://github.com/alibaba/arthas) 诊断命令（watch/trace/jad 等）。

### 工作区
- 多项目根目录、最近项目列表、带完整增删改查（创建 / 重命名 / 复制 / 删除）的文件树、模糊**快速打开**、以及**全局文件搜索**。
- 内置**性能监视器**，报告可导出。

---

## 技术栈

| 层 | 技术 |
| --- | --- |
| 前端 | TypeScript、Vite 6、CodeMirror 6、xterm.js、marked |
| 后端 | Rust、Tauri 2、tokio、`notify`（文件系统监听）、`portable-pty`（终端）、`walkdir`、`rayon`、`quick-xml`（pom 解析）、`reqwest`（HTTP） |
| 插件 | `tauri-plugin-shell`、`tauri-plugin-dialog` |

---

## 项目结构

```
lite-editor/
├── index.html              # 应用外壳
├── src/                    # 前端（TypeScript）
│   ├── main.ts             # 入口、菜单接线、文件监听重载
│   ├── editor-setup.ts     # CodeMirror 状态、自动保存、外部重载
│   ├── tabs.ts             # 标签管理器
│   ├── state.ts            # 全局应用状态 + LRU 编辑器缓存
│   ├── git-panel.ts        # Git 界面
│   ├── lsp-*.ts            # LSP 客户端与导航
│   ├── maven-helper.ts     # Maven 面板
│   ├── terminal-panel.ts   # 集成终端
│   ├── md-preview.ts       # Markdown 预览
│   └── ...
├── src-tauri/              # 后端（Rust）
│   ├── src/
│   │   ├── lib.rs          # Tauri 构建器、原生菜单、命令注册
│   │   ├── commands.rs     # 文件操作、搜索、Maven、文件监听、会话
│   │   ├── git.rs          # Git 命令面
│   │   ├── lsp.rs          # Language Server Protocol 桥接
│   │   ├── java_index.rs   # Java 类索引器
│   │   └── terminal.rs     # PTY 管理
│   └── tests/              # Rust 集成测试
└── package.json
```

---

## 快速开始

### 前置条件
- [Node.js](https://nodejs.org/) 18+
- [Rust](https://rustup.rs/)（stable 工具链）
- Tauri 2 的平台前置依赖——参见 [Tauri 前置条件指南](https://tauri.app/start/prerequisites/)。

### 安装
```bash
npm install
```

### 开发模式运行
```bash
npm run tauri dev
```

### 构建原生安装包
```bash
npm run tauri build
```

仅前端可单独类型检查并构建：`npm run build`（即 `tsc && vite build`）。Rust 测试从 `src-tauri/` 运行：
```bash
cd src-tauri && cargo test
```

---

## 快捷键

| 操作 | 快捷键 |
| --- | --- |
| 打开文件夹 | `Cmd/Ctrl` + `O` |
| 保存 | `Cmd/Ctrl` + `S` |
| 关闭标签 | `Cmd/Ctrl` + `W` |
| 全局文件搜索 | `Cmd/Ctrl` + `Shift` + `F` |
| 快速打开文件 | 双击 `左 Shift` |
| 跳转到定义 | `F12` 或 `Cmd` + 点击 |
| 后退 / 前进导航 | `Cmd/Ctrl` + `[` / `]` |
| 导出性能报告 | `Cmd/Ctrl` + `Shift` + `P` |

终端、Git 面板从 **View（视图）** 菜单切换。

---

## 许可证

当前未指定开源许可证；项目标记为 `private`。在添加 `LICENSE` 文件之前，请视为保留所有权利（all-rights-reserved）。
