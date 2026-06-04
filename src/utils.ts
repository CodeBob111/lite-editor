export function escapeHtml(text: string): string {
  const el = document.createElement("span");
  el.textContent = text;
  return el.innerHTML;
}

export function showStatus(message: string, isError = false) {
  const el = document.getElementById("status-text")!;
  el.textContent = message;
  el.style.color = isError ? "var(--danger)" : "var(--text-muted)";
  if (!isError) {
    setTimeout(() => {
      if (el.textContent === message) el.textContent = "Ready";
    }, 3000);
  }
}

export function positionContextMenu(menu: HTMLElement, x: number, y: number) {
  menu.style.top = "-9999px";
  menu.style.left = "-9999px";
  menu.style.maxHeight = "none";
  menu.classList.remove("hidden");

  const fullH = menu.offsetHeight;
  const fullW = menu.offsetWidth;
  const vh = window.innerHeight;
  const vw = window.innerWidth;
  const pad = 8;

  let top: number;
  if (y + fullH + pad <= vh) {
    top = y;
  } else if (y - fullH - pad >= 0) {
    top = y - fullH;
  } else {
    top = pad;
  }
  const availH = vh - top - pad;
  if (fullH > availH) {
    menu.style.maxHeight = `${availH}px`;
  }

  let left = x;
  if (left + fullW + pad > vw) {
    left = Math.max(pad, vw - fullW - pad);
  }

  menu.style.top = `${top}px`;
  menu.style.left = `${left}px`;
}

export function abbreviatePath(fullPath: string): string {
  const homeMatch = fullPath.match(/^(\/Users\/[^/]+)/);
  if (homeMatch) {
    return fullPath.replace(homeMatch[1], "~");
  }
  return fullPath;
}

export function getLanguageId(filename: string): string {
  if (filename.endsWith(".java")) return "java";
  if (filename.endsWith(".py")) return "python";
  if (filename.endsWith(".ts") || filename.endsWith(".tsx")) return "typescript";
  if (filename.endsWith(".js") || filename.endsWith(".jsx")) return "javascript";
  return "plaintext";
}

export function fuzzyMatch(query: string, target: string): number {
  const q = query.toLowerCase();
  const t = target.toLowerCase();

  if (q.length === 0) return 1;
  if (q.length > t.length) return 0;

  let qi = 0;
  let score = 0;
  let consecutive = 0;

  for (let ti = 0; ti < t.length && qi < q.length; ti++) {
    if (t[ti] === q[qi]) {
      qi++;
      consecutive++;
      score += consecutive;
      if (ti === 0 || "/.-_".includes(t[ti - 1])) {
        score += 10;
      }
    } else {
      consecutive = 0;
    }
  }

  if (qi < q.length) return 0;

  score += Math.max(0, 100 - t.length);

  const filename = target.split("/").pop() || target;
  if (filename.toLowerCase().includes(q)) {
    score += 30;
  }
  if (filename.toLowerCase().startsWith(q)) {
    score += 20;
  }

  return score;
}
