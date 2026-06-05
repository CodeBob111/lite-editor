import { EditorView, type Panel, type ViewUpdate } from "@codemirror/view";
import {
  SearchQuery,
  getSearchQuery,
  setSearchQuery,
  findNext,
  findPrevious,
  closeSearchPanel,
} from "@codemirror/search";

// 计数软上限:超大文档里命中数极多时,全量统计会卡顿,数到这里就停并显示 "N+"。
const COUNT_CAP = 9999;

function makeToggle(label: string, title: string, active: boolean): HTMLButtonElement {
  const b = document.createElement("button");
  b.type = "button";
  b.className = "cm-idea-search-toggle" + (active ? " active" : "");
  b.textContent = label;
  b.title = title;
  b.tabIndex = -1; // 不抢输入框焦点
  return b;
}

function makeNav(label: string, title: string): HTMLButtonElement {
  const b = document.createElement("button");
  b.type = "button";
  b.className = "cm-idea-search-nav";
  b.textContent = label;
  b.title = title;
  b.tabIndex = -1;
  return b;
}

// 自定义 Cmd+F 查找面板,样式对齐 IntelliJ IDEA 的查找条:
// 搜索框 + Cc(区分大小写)/W(全词)/.*(正则) 图标开关 + 命中计数 + 上/下一个 + 关闭。
// 通过 search({ top: true, createPanel: createSearchPanel }) 接入(见 editor-setup.ts)。
export function createSearchPanel(view: EditorView): Panel {
  const wrap = document.createElement("div");
  wrap.className = "cm-idea-search";

  const icon = document.createElement("span");
  icon.className = "cm-idea-search-icon";
  icon.textContent = "⌕"; // ⌕

  // 主输入框必须带 main-field=true,CM 打开面板时才会聚焦它。
  const input = document.createElement("input");
  input.className = "cm-idea-search-input";
  input.setAttribute("main-field", "true");
  input.type = "text";
  input.placeholder = "Find";
  input.setAttribute("aria-label", "Find");

  let initial = getSearchQuery(view.state);
  // 还没有查询、但编辑器里选中了一段单行文本时,用它作初始关键词(对齐 IDEA)。
  if (!initial.search) {
    const sel = view.state.selection.main;
    if (!sel.empty) {
      const text = view.state.sliceDoc(sel.from, sel.to);
      if (text && !text.includes("\n") && text.length <= 100) {
        initial = new SearchQuery({
          search: text,
          caseSensitive: initial.caseSensitive,
          wholeWord: initial.wholeWord,
          regexp: initial.regexp,
        });
      }
    }
  }
  input.value = initial.search;

  const caseBtn = makeToggle("Cc", "Match Case", initial.caseSensitive);
  const wordBtn = makeToggle("W", "Words", initial.wholeWord);
  const reBtn = makeToggle(".*", "Regex", initial.regexp);

  const count = document.createElement("span");
  count.className = "cm-idea-search-count";

  const prevBtn = makeNav("↑", "Previous match (Shift+Enter)"); // ↑
  const nextBtn = makeNav("↓", "Next match (Enter)"); // ↓
  const closeBtn = makeNav("✕", "Close (Esc)"); // ✕
  closeBtn.classList.add("cm-idea-search-close");

  wrap.append(icon, input, caseBtn, wordBtn, reBtn, count, prevBtn, nextBtn, closeBtn);

  // 命中计数防抖:超大文件里搜常见字符时,getCursor 全量扫描可能每次按键都跑一遍、
  // 拖慢输入。高亮(setSearchQuery)保持即时,只把计数延后 150ms。
  let countTimer: ReturnType<typeof setTimeout> | null = null;
  function scheduleCount(query: SearchQuery) {
    if (countTimer) clearTimeout(countTimer);
    countTimer = setTimeout(() => { countTimer = null; updateCount(query); }, 150);
  }

  // 统计命中数。正则非法时 query.valid 为 false。
  function updateCount(query: SearchQuery) {
    count.classList.remove("empty", "error");
    if (!query.search) {
      count.textContent = "";
      return;
    }
    if (!query.valid) {
      count.textContent = "Bad pattern";
      count.classList.add("error");
      return;
    }
    let n = 0;
    const cursor = query.getCursor(view.state);
    while (!cursor.next().done) {
      if (++n >= COUNT_CAP) break;
    }
    if (n === 0) {
      count.textContent = "No results";
      count.classList.add("empty");
    } else if (n >= COUNT_CAP) {
      count.textContent = `${COUNT_CAP}+`;
    } else {
      count.textContent = `${n} result${n === 1 ? "" : "s"}`;
    }
  }

  // 把 UI 当前状态打包成 SearchQuery 提交给 CM(驱动高亮 + findNext/Previous)。
  // 计数不在这里做,统一交给下面的 update():每次 setSearchQuery 派发都会触发它,
  // 避免一次按键算两遍。
  function commit() {
    const query = new SearchQuery({
      search: input.value,
      caseSensitive: caseBtn.classList.contains("active"),
      wholeWord: wordBtn.classList.contains("active"),
      regexp: reBtn.classList.contains("active"),
    });
    view.dispatch({ effects: setSearchQuery.of(query) });
  }

  input.addEventListener("input", commit);
  input.addEventListener("keydown", (e) => {
    if (e.key === "Enter") {
      e.preventDefault();
      if (e.shiftKey) findPrevious(view);
      else findNext(view);
    } else if (e.key === "Escape") {
      e.preventDefault();
      closeSearchPanel(view);
      view.focus();
    }
  });

  for (const btn of [caseBtn, wordBtn, reBtn]) {
    btn.addEventListener("click", (e) => {
      e.preventDefault();
      btn.classList.toggle("active");
      commit();
      input.focus();
    });
  }
  prevBtn.addEventListener("click", (e) => { e.preventDefault(); findPrevious(view); input.focus(); });
  nextBtn.addEventListener("click", (e) => { e.preventDefault(); findNext(view); input.focus(); });
  closeBtn.addEventListener("click", (e) => { e.preventDefault(); closeSearchPanel(view); view.focus(); });

  return {
    top: true,
    dom: wrap,
    mount() {
      input.select();
      // 有初始关键词(含选区种子)时提交,让高亮与计数生效。延后一帧再 dispatch:
      // mount 处于 view 更新周期内,同步派发事务会重入。无关键词则直接刷新计数(只读,安全)。
      if (input.value) requestAnimationFrame(() => { if (input.value) commit(); });
      else updateCount(getSearchQuery(view.state));
    },
    update(update: ViewUpdate) {
      // 外部修改查询(如其它命令)时同步 UI;文档变更时重算命中数。
      let queryChanged = false;
      for (const tr of update.transactions) {
        for (const ef of tr.effects) {
          if (ef.is(setSearchQuery)) {
            const q = ef.value;
            if (document.activeElement !== input) input.value = q.search;
            caseBtn.classList.toggle("active", q.caseSensitive);
            wordBtn.classList.toggle("active", q.wholeWord);
            reBtn.classList.toggle("active", q.regexp);
            queryChanged = true;
          }
        }
      }
      if (queryChanged || update.docChanged) scheduleCount(getSearchQuery(update.state));
    },
    destroy() {
      if (countTimer) clearTimeout(countTimer);
    },
  };
}
