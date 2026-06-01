const MAX_ENTRIES = 2000;
const SLOW_THRESHOLD_MS = 50;

export interface PerfEntry {
  ts: number;
  kind: "ipc" | "ui" | "longtask";
  label: string;
  ms: number;
  args?: string;
}

const entries: PerfEntry[] = [];
let enabled = true;

export function perfEnabled() { return enabled; }
export function setPerfEnabled(v: boolean) { enabled = v; }

export function record(entry: PerfEntry) {
  if (!enabled) return;
  if (entry.ms < SLOW_THRESHOLD_MS && entry.kind !== "longtask") return;
  entries.push(entry);
  if (entries.length > MAX_ENTRIES) entries.splice(0, entries.length - MAX_ENTRIES);
}

export async function trackAsync<T>(kind: PerfEntry["kind"], label: string, fn: () => Promise<T>, args?: string): Promise<T> {
  if (!enabled) return fn();
  const t0 = performance.now();
  try {
    return await fn();
  } finally {
    record({ ts: Date.now(), kind, label, ms: performance.now() - t0, args });
  }
}

export function trackSync(kind: PerfEntry["kind"], label: string, fn: () => void, args?: string) {
  if (!enabled) return fn();
  const t0 = performance.now();
  try {
    return fn();
  } finally {
    record({ ts: Date.now(), kind, label, ms: performance.now() - t0, args });
  }
}

export function getEntries(): readonly PerfEntry[] { return entries; }
export function clearEntries() { entries.length = 0; }

export function formatReport(): string {
  if (entries.length === 0) return "No slow operations recorded.\n";
  const lines: string[] = [
    `Perf Report — ${new Date().toISOString()} — ${entries.length} entries (threshold: ${SLOW_THRESHOLD_MS}ms)`,
    "─".repeat(100),
    pad("Time", 24) + pad("Kind", 10) + pad("Duration", 12) + pad("Label", 36) + "Args",
    "─".repeat(100),
  ];
  for (const e of entries) {
    const time = new Date(e.ts).toISOString().slice(11, 23);
    const ms = e.ms.toFixed(1) + "ms";
    lines.push(pad(time, 24) + pad(e.kind, 10) + pad(ms, 12) + pad(e.label, 36) + (e.args || ""));
  }

  lines.push("─".repeat(100));

  const byLabel = new Map<string, { count: number; total: number; max: number }>();
  for (const e of entries) {
    const key = `${e.kind}:${e.label}`;
    const s = byLabel.get(key) || { count: 0, total: 0, max: 0 };
    s.count++;
    s.total += e.ms;
    if (e.ms > s.max) s.max = e.ms;
    byLabel.set(key, s);
  }
  lines.push("\nSummary (sorted by total time):");
  lines.push(pad("Label", 40) + pad("Count", 8) + pad("Total", 12) + pad("Avg", 12) + "Max");
  const sorted = [...byLabel.entries()].sort((a, b) => b[1].total - a[1].total);
  for (const [key, s] of sorted) {
    lines.push(
      pad(key, 40) +
      pad(String(s.count), 8) +
      pad(s.total.toFixed(1) + "ms", 12) +
      pad((s.total / s.count).toFixed(1) + "ms", 12) +
      s.max.toFixed(1) + "ms"
    );
  }

  return lines.join("\n") + "\n";
}

function pad(s: string, n: number) { return s.length >= n ? s + " " : s + " ".repeat(n - s.length); }

export function initLongTaskObserver() {
  if (typeof PerformanceObserver === "undefined") return;
  try {
    const obs = new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) {
        record({
          ts: Date.now(),
          kind: "longtask",
          label: (entry as any).name || "self",
          ms: entry.duration,
        });
      }
    });
    obs.observe({ type: "longtask", buffered: false });
  } catch {
    // longtask not supported in this engine
  }
}
