// jdtls 常驻数 LRU 的行为回归测试:上限 3、超限淘汰最久未用,
// 但「该根下仍有打开的 Java 编辑器视图」的根不淘汰(否则正在用的诊断/跳转悄悄失效)。
import { describe, it, expect, vi, beforeEach } from "vitest";

vi.mock("./tauri-api", () => ({
  startLsp: vi.fn(() => Promise.resolve()),
  stopLsp: vi.fn(() => Promise.resolve()),
  onLspProgress: vi.fn(),
  parseMavenModules: vi.fn(() => Promise.resolve([])),
  onMavenOutput: vi.fn(),
  onMavenDone: vi.fn(),
  buildJavaIndex: vi.fn(() => Promise.resolve(0)),
  loadJavaIndex: vi.fn(() => Promise.resolve(0)),
  buildUsageIndex: vi.fn(() => Promise.resolve(0)),
  gitDiscoverRepos: vi.fn(),
}));

import { ensureJavaLspForFile } from "./lsp-manager";
import { startLsp, stopLsp, gitDiscoverRepos } from "./tauri-api";
import { app } from "./state";
import type { EditorView } from "@codemirror/view";

const PROJECT = "/ws";
const REPOS = ["/ws/r1", "/ws/r2", "/ws/r3", "/ws/r4"];

beforeEach(() => {
  vi.mocked(startLsp).mockClear();
  vi.mocked(stopLsp).mockClear();
  vi.mocked(gitDiscoverRepos).mockResolvedValue(REPOS.map((p) => ({ name: p, path: p })));
  app.currentProjectPath = PROJECT;
  app.editorViewCache.clear();
});

describe("jdtls LRU", () => {
  it("超过 3 个常驻实例时,关掉最久未用且不在用的", async () => {
    for (const repo of REPOS) {
      await ensureJavaLspForFile(`${repo}/src/A.java`);
    }
    // r1 最久未用且没有打开的 Java 视图 → 应被淘汰
    expect(vi.mocked(stopLsp).mock.calls).toContainEqual(["java", "/ws/r1"]);
    // 最近使用的 r4 绝不能被淘汰
    expect(vi.mocked(stopLsp).mock.calls).not.toContainEqual(["java", "/ws/r4"]);
  });

  it("根下仍有打开的 Java 视图时不淘汰它,宁可暂时超限", async () => {
    // r1 有一个打开的 Java 编辑器视图(模拟正在使用)
    app.editorViewCache.set("/ws/r1/src/InUse.java", { destroy: vi.fn() } as unknown as EditorView);
    for (const repo of REPOS) {
      await ensureJavaLspForFile(`${repo}/src/A.java`);
    }
    expect(vi.mocked(stopLsp).mock.calls).not.toContainEqual(["java", "/ws/r1"]); // 在用 → 保留
    app.editorViewCache.clear();
  });
});
