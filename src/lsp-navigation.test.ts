// debouncedLspDidChange 的回归测试:必须按文件各自防抖。
// 曾经的真 bug:全局共用一个 timer,700ms 内编辑/切换另一个文件会把上一个文件
// 待发的 didChange 取消掉,LSP 留下过期内容(诊断/跳转随之错乱)。
import { describe, it, expect, vi, beforeEach, afterEach } from "vitest";

vi.mock("./tauri-api", () => ({
  lspIsReady: vi.fn(),
  lspGotoDefinition: vi.fn(),
  lspFindReferences: vi.fn(),
  findClassInMaven: vi.fn(),
  searchJavaClass: vi.fn(),
  lspDidChange: vi.fn(() => Promise.resolve()),
  searchInFiles: vi.fn(),
  listAllFiles: vi.fn(),
  readFile: vi.fn(),
  queryUsages: vi.fn(),
}));

import { debouncedLspDidChange } from "./lsp-navigation";
import { lspDidChange } from "./tauri-api";

beforeEach(() => {
  vi.useFakeTimers();
  vi.mocked(lspDidChange).mockClear();
});
afterEach(() => {
  vi.useRealTimers();
});

describe("debouncedLspDidChange", () => {
  it("不同文件的防抖互不取消:两个文件的 didChange 都必须送达", () => {
    debouncedLspDidChange("/a.java", () => "content-A");
    vi.advanceTimersByTime(300); // 700ms 内
    debouncedLspDidChange("/b.java", () => "content-B");
    vi.advanceTimersByTime(800);

    const calls = vi.mocked(lspDidChange).mock.calls;
    expect(calls).toContainEqual(["/a.java", "content-A"]);
    expect(calls).toContainEqual(["/b.java", "content-B"]);
  });

  it("同一文件连续编辑只送最后一版", () => {
    debouncedLspDidChange("/a.java", () => "v1");
    vi.advanceTimersByTime(300);
    debouncedLspDidChange("/a.java", () => "v2");
    vi.advanceTimersByTime(800);

    const calls = vi.mocked(lspDidChange).mock.calls.filter((c) => c[0] === "/a.java");
    expect(calls).toEqual([["/a.java", "v2"]]);
  });
});
