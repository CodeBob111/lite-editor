// 前端核心状态容器的回归测试:编辑器视图 LRU(上限 30、淘汰必须 destroy)与
// 诊断表上限(200)。这两处是前端内存有界性的承重墙,改坏会直接退化为无界增长。
import { describe, it, expect, vi } from "vitest";
import { app } from "./state";
import type { EditorView } from "@codemirror/view";

function fakeView(): EditorView {
  return { destroy: vi.fn() } as unknown as EditorView;
}

describe("LruEditorCache", () => {
  it("超过 30 个视图时淘汰最久未用并 destroy", () => {
    app.editorViewCache.clear();
    const views: EditorView[] = [];
    for (let i = 0; i < 31; i++) {
      const v = fakeView();
      views.push(v);
      app.editorViewCache.set(`/f${i}.java`, v);
    }
    expect(app.editorViewCache.size).toBe(30);
    expect(app.editorViewCache.has("/f0.java")).toBe(false);
    expect((views[0] as any).destroy).toHaveBeenCalled(); // 淘汰必须销毁,否则视图泄漏
    expect(app.editorViewCache.has("/f30.java")).toBe(true);
    app.editorViewCache.clear();
  });

  it("get 刷新最近使用序,刚访问过的不被淘汰", () => {
    app.editorViewCache.clear();
    for (let i = 0; i < 30; i++) app.editorViewCache.set(`/g${i}.java`, fakeView());
    app.editorViewCache.get("/g0.java"); // 触摸最旧的
    app.editorViewCache.set("/g30.java", fakeView());
    expect(app.editorViewCache.has("/g0.java")).toBe(true); // 被触摸过 → 保留
    expect(app.editorViewCache.has("/g1.java")).toBe(false); // 改淘汰它
    app.editorViewCache.clear();
  });

  it("同 key 覆盖时销毁旧视图", () => {
    app.editorViewCache.clear();
    const old = fakeView();
    app.editorViewCache.set("/dup.java", old);
    app.editorViewCache.set("/dup.java", fakeView());
    expect((old as any).destroy).toHaveBeenCalled();
    expect(app.editorViewCache.size).toBe(1);
    app.editorViewCache.clear();
  });
});

describe("BoundedDiagnosticsMap", () => {
  it("超过 200 条时淘汰最早的", () => {
    app.diagnosticsMap.clear();
    for (let i = 0; i <= 200; i++) {
      app.diagnosticsMap.set(`file:///d${i}.java`, []);
    }
    expect(app.diagnosticsMap.has("file:///d0.java")).toBe(false);
    expect(app.diagnosticsMap.has("file:///d200.java")).toBe(true);
    app.diagnosticsMap.clear();
  });
});
