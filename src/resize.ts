export function setupResizeHandles() {
  const appEl = document.getElementById("app")!;

  const sidebarHandle = document.getElementById("resize-handle-sidebar")!;
  let sidebarDragging = false;
  let panelBaseTop = 0;
  let panelBaseHeight = 0;
  let pendingMouseEvent: MouseEvent | null = null;
  let resizeFrame = 0;

  sidebarHandle.addEventListener("mousedown", (e) => {
    e.preventDefault();
    sidebarDragging = true;
    sidebarHandle.classList.add("dragging");
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
  });

  const panelHandle = document.getElementById("resize-handle-panel")!;
  let panelDragging = false;

  panelHandle.addEventListener("mousedown", (e) => {
    e.preventDefault();
    panelDragging = true;
    const rect = appEl.getBoundingClientRect();
    panelBaseTop = rect.top;
    panelBaseHeight = rect.height;
    panelHandle.classList.add("dragging");
    document.body.style.cursor = "row-resize";
    document.body.style.userSelect = "none";
  });

  function applyResize() {
    resizeFrame = 0;
    const e = pendingMouseEvent;
    if (!e) return;

    if (sidebarDragging) {
      const width = Math.max(150, Math.min(600, e.clientX));
      appEl.style.setProperty("--sidebar-width", `${width}px`);
    }
    if (panelDragging) {
      const panelHeight = Math.max(80, Math.min(panelBaseHeight - 100, panelBaseHeight - e.clientY + panelBaseTop));
      appEl.style.setProperty("--panel-height", `${panelHeight}px`);
    }
  }

  document.addEventListener("mousemove", (e) => {
    if (!sidebarDragging && !panelDragging) return;
    pendingMouseEvent = e;
    if (!resizeFrame) resizeFrame = requestAnimationFrame(applyResize);
  });

  document.addEventListener("mouseup", () => {
    if (sidebarDragging) {
      sidebarDragging = false;
      sidebarHandle.classList.remove("dragging");
    }
    if (panelDragging) {
      panelDragging = false;
      panelHandle.classList.remove("dragging");
    }
    pendingMouseEvent = null;
    if (resizeFrame) {
      cancelAnimationFrame(resizeFrame);
      resizeFrame = 0;
    }
    document.body.style.cursor = "";
    document.body.style.userSelect = "";
  });
}
