import { app } from "./state";
import { escapeHtml, showStatus, positionContextMenu } from "./utils";
import { appPrompt, appConfirm } from "./dialog";
import {
  saveFileDialog,
  writeFile,
  gitDiscoverRepos, gitCurrentBranch, gitCurrentBranchBatch, gitListBranches,
  gitCheckout, gitNewBranch, gitDeleteBranch,
  gitPull, gitPrune, gitPush, gitMerge, gitRebase, gitRenameBranch, gitFetchBranch,
  gitLog, gitCommitPatch, gitCherryPick, gitCheckoutRevision, gitRevertCommit, gitNewBranchAtCommit,
  type GitBranch, type GitCommit, type GitRepo,
} from "./tauri-api";
import { showConflictsDialog } from "./merge-conflict";

let gitRepos: GitRepo[] = [];
let gitExpandedRepos = new Set<string>();
let gitRepoHeadCache = new Map<string, string>();
let gitRepoBranchCache = new Map<string, { branches: GitBranch[]; error: string | null }>();
let gitContextBranch: GitBranch | null = null;
let gitContextRepo: string | null = null;
let gitContextCommit: GitCommit | null = null;
let gitContextCommitRepo: string | null = null;
let gitActiveLogRepo: string | null = null;
let gitActiveLogBranch: string | null = null;
let gitActiveLogCommits: GitCommit[] = [];
let gitLogToken = 0;
let loadGitInFlight: Promise<void> | null = null;
let loadGitInFlightProject = "";
let loadGitToken = 0;
let gitLoadedAt = 0;
let gitLoadedProject = "";
const GIT_CACHE_MS = 5000;
const LOCAL_BRANCH_RENDER_LIMIT = 300;
const REMOTE_BRANCH_RENDER_LIMIT = 80;
const GIT_LOG_LIMIT = 250;

function renderBranchBadges(b: GitBranch): string {
  let badges = "";
  if (b.upstream_gone) {
    badges += `<span class="git-badge git-badge-gone" title="Upstream branch no longer exists">gone</span>`;
  } else {
    if (b.behind > 0) badges += `<span class="git-badge git-badge-behind" title="${b.behind} commit(s) behind">&darr;${b.behind}</span>`;
    if (b.ahead > 0) badges += `<span class="git-badge git-badge-ahead" title="${b.ahead} commit(s) ahead">&uarr;${b.ahead}</span>`;
  }
  if (!b.remote && !b.tracking) badges += `<span class="git-badge git-badge-notrack" title="No tracking branch">untracked</span>`;
  return badges;
}

function branchDataAttrs(repoPath: string, branch: GitBranch): string {
  return [
    `data-branch="${escapeHtml(branch.name)}"`,
    `data-repo="${escapeHtml(repoPath)}"`,
    `data-remote="${branch.remote}"`,
    `data-current="${branch.current}"`,
    `data-tracking="${escapeHtml(branch.tracking ?? "")}"`,
    `data-ahead="${branch.ahead}"`,
    `data-behind="${branch.behind}"`,
    `data-upstream-gone="${branch.upstream_gone}"`,
  ].join(" ");
}

function findCachedBranch(repoPath: string, branchName: string, remote: boolean): GitBranch | null {
  const cached = gitRepoBranchCache.get(repoPath);
  return cached?.branches.find((b) => b.name === branchName && b.remote === remote) ?? null;
}

function activeRepoForCurrentFile(): string | null {
  if (!app.currentFilePath) return null;
  for (const r of gitRepos) {
    if (app.currentFilePath.startsWith(r.path + "/")) return r.path;
  }
  return null;
}

function remoteFromTracking(tracking: string | null): string {
  return tracking?.split("/")[0] || "origin";
}

function branchRank(branch: GitBranch) {
  if (branch.current) return 0;
  if (branch.name === "master") return 1;
  if (branch.name === "main") return 2;
  return 10;
}

function branchItemClass(repoPath: string, branch: GitBranch): string {
  const classes = ["git-branch-item"];
  if (branch.current) classes.push("current");
  if (gitActiveLogRepo === repoPath && gitActiveLogBranch === branch.name) classes.push("log-selected");
  return classes.join(" ");
}

function sortLocalBranches(branches: GitBranch[]) {
  return [...branches].sort((a, b) => {
    const rank = branchRank(a) - branchRank(b);
    if (rank !== 0) return rank;
    return a.name.localeCompare(b.name);
  });
}

function sortedRemoteBranches(branches: GitBranch[]) {
  return [...branches].sort((a, b) => a.name.localeCompare(b.name));
}

function formatCommitDate(value: string): string {
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  const now = new Date();
  const sameYear = date.getFullYear() === now.getFullYear();
  const datePart = sameYear
    ? `${date.getMonth() + 1}/${date.getDate()}`
    : `${date.getFullYear()}/${date.getMonth() + 1}/${date.getDate()}`;
  const timePart = date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit", hour12: false });
  return `${datePart} ${timePart}`;
}

function renderCommitRefs(refs: string): string {
  if (!refs) return "";
  return refs
    .split(", ")
    .filter(Boolean)
    .slice(0, 4)
    .map((ref) => `<span class="git-log-ref">${escapeHtml(ref.replace(/^tag: /, ""))}</span>`)
    .join("");
}

function renderGitLogEmpty(message: string) {
  const log = document.getElementById("git-log")!;
  log.innerHTML = `<div class="git-log-empty">${escapeHtml(message)}</div>`;
}

function updateGitLogTitle(repoPath: string | null, branch: string | null) {
  const title = document.getElementById("git-log-title");
  if (!title) return;
  if (!repoPath || !branch) {
    title.textContent = "";
    title.title = "";
    return;
  }
  const repoName = repoPath.split("/").pop() || repoPath;
  title.textContent = `${repoName}: ${branch}`;
  title.title = `${repoPath} - ${branch}`;
}

function renderGitLog(repoPath: string, branch: string, commits: GitCommit[]) {
  const log = document.getElementById("git-log")!;
  if (commits.length === 0) {
    renderGitLogEmpty("No commits");
    return;
  }
  gitActiveLogCommits = commits;
  const html = commits
    .map((commit, index) => {
      const lane = Math.min(commit.parents.length || 1, 4);
      const mergeClass = commit.parents.length > 1 ? " merge" : "";
      return `<div class="git-log-row${mergeClass}" data-repo="${escapeHtml(repoPath)}" data-hash="${escapeHtml(commit.hash)}">
        <div class="git-log-graph lane-${lane}">
          <span class="git-log-line"></span>
          <span class="git-log-dot"></span>
          ${index === 0 ? "" : '<span class="git-log-line-top"></span>'}
        </div>
        <div class="git-log-message" title="${escapeHtml(commit.subject)}">${escapeHtml(commit.subject)}</div>
        <div class="git-log-refs">${renderCommitRefs(commit.refs)}</div>
        <div class="git-log-author" title="${escapeHtml(commit.author)}">${escapeHtml(commit.author)}</div>
        <div class="git-log-date">${escapeHtml(formatCommitDate(commit.date))}</div>
      </div>`;
    })
    .join("");
  log.innerHTML = html;
}

async function showBranchLog(repoPath: string, branch: string) {
  gitActiveLogRepo = repoPath;
  gitActiveLogBranch = branch;
  gitActiveLogCommits = [];
  updateGitLogTitle(repoPath, branch);
  renderGitLogEmpty("Loading commits...");
  document
    .querySelectorAll(".git-branch-item.log-selected")
    .forEach((el) => el.classList.remove("log-selected"));
  document
    .querySelector<HTMLElement>(
      `.git-branch-item[data-repo="${CSS.escape(repoPath)}"][data-branch="${CSS.escape(branch)}"]`,
    )
    ?.classList.add("log-selected");

  const token = ++gitLogToken;
  try {
    const commits = await gitLog(repoPath, branch, GIT_LOG_LIMIT);
    if (token !== gitLogToken || gitActiveLogRepo !== repoPath || gitActiveLogBranch !== branch) return;
    renderGitLog(repoPath, branch, commits);
  } catch (err) {
    if (token !== gitLogToken) return;
    renderGitLogEmpty(`Failed to load commits: ${String(err)}`);
  }
}

function renderGitPanel() {
  const container = document.getElementById("git-branches")!;
  if (gitRepos.length === 0) {
    container.innerHTML = '<div class="panel-placeholder">No git repositories found</div>';
    return;
  }

  let html = "";
  for (const repo of gitRepos) {
    const expanded = gitExpandedRepos.has(repo.path);
    const branchLabel = gitRepoHeadCache.get(repo.path) ?? "...";

    html += `<div class="git-repo-section" data-repo="${escapeHtml(repo.path)}">`;
    html += `<div class="git-repo-header" data-repo="${escapeHtml(repo.path)}">`;
    html += `<span class="git-repo-toggle">${expanded ? "&#9660;" : "&#9654;"}</span>`;
    html += `<span class="git-repo-name">${escapeHtml(repo.name)}</span>`;
    html += `<span class="git-repo-branch">${escapeHtml(branchLabel)}</span>`;
    html += `</div>`;

    if (expanded) {
      const cached = gitRepoBranchCache.get(repo.path);
      if (!cached) {
        html += `<div class="git-repo-error">Loading...</div>`;
      } else if (cached.error) {
        html += `<div class="git-repo-error">${escapeHtml(cached.error)}</div>`;
      } else {
        const local = sortLocalBranches(cached.branches.filter((b) => !b.remote));
        const remote = sortedRemoteBranches(cached.branches.filter((b) => b.remote));
        const visibleLocal = local.slice(0, LOCAL_BRANCH_RENDER_LIMIT);
        const visibleRemote = remote.slice(0, REMOTE_BRANCH_RENDER_LIMIT);

        html += '<div class="git-repo-branches">';
        for (const b of visibleLocal) {
          const icon = b.current ? "&#9733;" : "&#9741;";
          html += `<div class="${branchItemClass(repo.path, b)}" ${branchDataAttrs(repo.path, b)} title="${escapeHtml(b.name)}"><span class="git-branch-icon">${icon}</span><span class="git-branch-name">${escapeHtml(b.name)}</span>${renderBranchBadges(b)}</div>`;
        }
        if (local.length > visibleLocal.length) {
          html += `<div class="git-section-header">Showing ${visibleLocal.length} of ${local.length} local branches</div>`;
        }
        if (remote.length > 0) {
          html += '<div class="git-section-header">Remote</div>';
          for (const b of visibleRemote) {
            html += `<div class="${branchItemClass(repo.path, b)}" ${branchDataAttrs(repo.path, b)} title="${escapeHtml(b.name)}"><span class="git-branch-icon">&#9741;</span><span class="git-branch-name">${escapeHtml(b.name)}</span></div>`;
          }
          if (remote.length > visibleRemote.length) {
            html += `<div class="git-section-header">Showing ${visibleRemote.length} of ${remote.length} remote branches</div>`;
          }
        }
        html += "</div>";
      }
    }
    html += "</div>";
  }

  container.innerHTML = html;
}

function renderGitRepoSection(repoPath: string) {
  const container = document.getElementById("git-branches")!;
  const section = container.querySelector<HTMLElement>(`.git-repo-section[data-repo="${CSS.escape(repoPath)}"]`);
  const repo = gitRepos.find((r) => r.path === repoPath);
  if (!section || !repo) return;

  const expanded = gitExpandedRepos.has(repo.path);
  const branchLabel = gitRepoHeadCache.get(repo.path) ?? "...";
  let html = `<div class="git-repo-header" data-repo="${escapeHtml(repo.path)}">`;
  html += `<span class="git-repo-toggle">${expanded ? "&#9660;" : "&#9654;"}</span>`;
  html += `<span class="git-repo-name">${escapeHtml(repo.name)}</span>`;
  html += `<span class="git-repo-branch">${escapeHtml(branchLabel)}</span>`;
  html += `</div>`;

  if (expanded) {
    const cached = gitRepoBranchCache.get(repo.path);
    if (!cached) {
      html += `<div class="git-repo-error">Loading...</div>`;
    } else if (cached.error) {
      html += `<div class="git-repo-error">${escapeHtml(cached.error)}</div>`;
    } else {
      const local = sortLocalBranches(cached.branches.filter((b) => !b.remote));
      const remote = sortedRemoteBranches(cached.branches.filter((b) => b.remote));
      const visibleLocal = local.slice(0, LOCAL_BRANCH_RENDER_LIMIT);
      const visibleRemote = remote.slice(0, REMOTE_BRANCH_RENDER_LIMIT);

      html += '<div class="git-repo-branches">';
      for (const b of visibleLocal) {
        const icon = b.current ? "&#9733;" : "&#9741;";
        html += `<div class="${branchItemClass(repo.path, b)}" ${branchDataAttrs(repo.path, b)} title="${escapeHtml(b.name)}"><span class="git-branch-icon">${icon}</span><span class="git-branch-name">${escapeHtml(b.name)}</span>${renderBranchBadges(b)}</div>`;
      }
      if (local.length > visibleLocal.length) {
        html += `<div class="git-section-header">Showing ${visibleLocal.length} of ${local.length} local branches</div>`;
      }
      if (remote.length > 0) {
        html += '<div class="git-section-header">Remote</div>';
        for (const b of visibleRemote) {
          html += `<div class="${branchItemClass(repo.path, b)}" ${branchDataAttrs(repo.path, b)} title="${escapeHtml(b.name)}"><span class="git-branch-icon">&#9741;</span><span class="git-branch-name">${escapeHtml(b.name)}</span></div>`;
        }
        if (remote.length > visibleRemote.length) {
          html += `<div class="git-section-header">Showing ${visibleRemote.length} of ${remote.length} remote branches</div>`;
        }
      }
      html += "</div>";
    }
  }

  section.innerHTML = html;
}

async function ensureRepoBranches(repoPath: string) {
  if (gitRepoBranchCache.has(repoPath)) return;
  try {
    const branches = await gitListBranches(repoPath);
    gitRepoBranchCache.set(repoPath, { branches, error: null });
    const current = branches.find((b) => b.current && !b.remote);
    if (current) gitRepoHeadCache.set(repoPath, current.name);
  } catch (err) {
    gitRepoBranchCache.set(repoPath, { branches: [], error: String(err) });
  }
}

export async function loadGitBranches() {
  if (!app.currentProjectPath) return;
  const now = performance.now();
  const projectPath = app.currentProjectPath;
  if (loadGitInFlight && loadGitInFlightProject === projectPath) return loadGitInFlight;
  if (gitLoadedProject !== projectPath) {
    gitRepos = [];
    gitRepoHeadCache.clear();
    gitRepoBranchCache.clear();
    gitActiveLogRepo = null;
    gitActiveLogBranch = null;
    gitActiveLogCommits = [];
    updateGitLogTitle(null, null);
    renderGitLogEmpty("Select a branch to view commits");
    document.getElementById("git-branches")!.innerHTML = '<div class="panel-placeholder">Loading git repositories...</div>';
  }
  if (gitLoadedProject === projectPath && gitRepos.length > 0 && now - gitLoadedAt < GIT_CACHE_MS) {
    renderGitPanel();
    return;
  }

  const token = ++loadGitToken;
  loadGitInFlightProject = projectPath;
  loadGitInFlight = (async () => {
    gitRepos = await gitDiscoverRepos(projectPath);
    if (token !== loadGitToken || app.currentProjectPath !== projectPath) return;
    gitLoadedProject = projectPath;
    if (gitRepos.length === 0) {
      renderGitPanel();
      return;
    }

    if (gitExpandedRepos.size === 0) {
      const activeRepo = activeRepoForCurrentFile();
      if (activeRepo) gitExpandedRepos.add(activeRepo);
      else if (gitRepos.length <= 3) gitRepos.forEach((r) => gitExpandedRepos.add(r.path));
      else gitExpandedRepos.add(gitRepos[0].path);
    }

    const repoPaths = new Set(gitRepos.map((r) => r.path));
    for (const key of gitRepoHeadCache.keys()) {
      if (!repoPaths.has(key)) gitRepoHeadCache.delete(key);
    }
    for (const key of gitRepoBranchCache.keys()) {
      if (!repoPaths.has(key)) gitRepoBranchCache.delete(key);
    }

    renderGitPanel();

    try {
      const batchResults = await gitCurrentBranchBatch(gitRepos.map((r) => r.path));
      for (const br of batchResults) {
        gitRepoHeadCache.set(br.path, br.result ?? "???");
      }
    } catch {
      for (const repo of gitRepos) gitRepoHeadCache.set(repo.path, "???");
    }
    if (token !== loadGitToken || app.currentProjectPath !== projectPath) return;
    for (const repo of gitRepos) renderGitRepoSection(repo.path);

    await Promise.all(
      gitRepos
        .filter((r) => gitExpandedRepos.has(r.path))
        .map(async (r) => {
          await ensureRepoBranches(r.path);
          if (token !== loadGitToken || app.currentProjectPath !== projectPath) return;
          renderGitRepoSection(r.path);
        }),
    );
    gitLoadedAt = performance.now();
  })().finally(() => {
    if (token === loadGitToken) {
      loadGitInFlight = null;
      loadGitInFlightProject = "";
    }
  });

  return loadGitInFlight;
}

async function refreshSingleRepo(repoPath: string, revealCurrent = false) {
  gitRepoBranchCache.delete(repoPath);
  try {
    gitRepoHeadCache.set(repoPath, await gitCurrentBranch(repoPath));
  } catch { /* keep old */ }
  if (gitExpandedRepos.has(repoPath)) await ensureRepoBranches(repoPath);
  renderGitRepoSection(repoPath);
  if (revealCurrent) {
    requestAnimationFrame(() => {
      const item = document.querySelector<HTMLElement>(
        `.git-repo-section[data-repo="${CSS.escape(repoPath)}"] .git-branch-item.current`,
      );
      item?.scrollIntoView({ block: "nearest" });
    });
  }
}

async function doGitCheckout(repo: string, branch: string) {
  try {
    await gitCheckout(repo, branch);
    showStatus(`[${repo.split("/").pop()}] Checked out ${branch}`);
    await refreshSingleRepo(repo, true);
  } catch (err) {
    const errStr = String(err);
    if (errStr.includes("would be overwritten") || errStr.includes("untracked working tree")) {
      if (await appConfirm(`Checkout blocked by untracked files. Force checkout '${branch}'?`)) {
        try {
          await gitCheckout(repo, branch, true);
          showStatus(`[${repo.split("/").pop()}] Force checked out ${branch}`);
          await refreshSingleRepo(repo, true);
        } catch (e2) {
          showStatus(`Force checkout failed: ${e2}`, true);
        }
      }
    } else {
      showStatus(`Checkout failed: ${errStr}`, true);
    }
  }
}

function hideGitContextMenu() {
  document.getElementById("git-context-menu")!.classList.add("hidden");
}

function hideGitCommitContextMenu() {
  document.getElementById("git-commit-context-menu")!.classList.add("hidden");
}

async function copyText(text: string) {
  await navigator.clipboard.writeText(text);
}

function safePatchName(commit: GitCommit) {
  const subject = commit.subject
    .toLowerCase()
    .replace(/[^a-z0-9._-]+/g, "-")
    .replace(/^-+|-+$/g, "")
    .slice(0, 60) || "commit";
  return `${commit.short_hash}-${subject}.patch`;
}

export function initGitPanel() {
  renderGitLogEmpty("Select a branch to view commits");

  document.getElementById("git-branches")!.addEventListener("click", async (e) => {
    const header = (e.target as HTMLElement).closest(".git-repo-header") as HTMLElement | null;
    if (header) {
      const repo = header.dataset.repo!;
      if (gitExpandedRepos.has(repo)) {
        gitExpandedRepos.delete(repo);
        renderGitRepoSection(repo);
      } else {
        gitExpandedRepos.add(repo);
        renderGitRepoSection(repo);
        if (!gitRepoBranchCache.has(repo)) {
          requestAnimationFrame(() => {
            ensureRepoBranches(repo).then(() => renderGitRepoSection(repo));
          });
        }
      }
      return;
    }

    const item = (e.target as HTMLElement).closest(".git-branch-item") as HTMLElement | null;
    if (!item) return;
    const branch = item.dataset.branch!;
    const repo = item.dataset.repo!;
    showBranchLog(repo, branch);
  });

  document.getElementById("git-branches")!.addEventListener("contextmenu", (e) => {
    const item = (e.target as HTMLElement).closest(".git-branch-item") as HTMLElement | null;
    if (!item) return;
    e.preventDefault();
    gitContextRepo = item.dataset.repo!;
    const remote = item.dataset.remote === "true";
    gitContextBranch = findCachedBranch(gitContextRepo, item.dataset.branch!, remote) ?? {
      name: item.dataset.branch!,
      current: item.dataset.current === "true",
      remote,
      ahead: Number(item.dataset.ahead ?? 0),
      behind: Number(item.dataset.behind ?? 0),
      tracking: item.dataset.tracking || null,
      upstream_gone: item.dataset.upstreamGone === "true",
    };
    positionContextMenu(document.getElementById("git-context-menu")!, e.clientX, e.clientY);
  });

  document.getElementById("git-log")!.addEventListener("click", (e) => {
    const row = (e.target as HTMLElement).closest(".git-log-row") as HTMLElement | null;
    if (!row) return;
    document.querySelectorAll(".git-log-row.selected").forEach((el) => el.classList.remove("selected"));
    row.classList.add("selected");
  });

  document.getElementById("git-log")!.addEventListener("contextmenu", (e) => {
    const row = (e.target as HTMLElement).closest(".git-log-row") as HTMLElement | null;
    if (!row) return;
    e.preventDefault();
    document.querySelectorAll(".git-log-row.selected").forEach((el) => el.classList.remove("selected"));
    row.classList.add("selected");
    gitContextCommitRepo = row.dataset.repo!;
    gitContextCommit = gitActiveLogCommits.find((c) => c.hash === row.dataset.hash) ?? null;
    if (!gitContextCommit) return;
    positionContextMenu(document.getElementById("git-commit-context-menu")!, e.clientX, e.clientY);
  });

  document.getElementById("git-context-menu")!.addEventListener("click", async (e) => {
    const action = (e.target as HTMLElement).dataset.action;
    if (!action || !gitContextBranch || !gitContextRepo) return;
    hideGitContextMenu();
    const branch = gitContextBranch.name;
    const repo = gitContextRepo;
    const repoName = repo.split("/").pop();

    try {
      switch (action) {
        case "git-checkout":
          doGitCheckout(repo, branch);
          return;
        case "git-new-branch": {
          const name = await appPrompt(`[${repoName}] New branch from '${branch}':`);
          if (name) {
            await gitNewBranch(repo, name, branch);
            showStatus(`[${repoName}] Created branch ${name}`);
          }
          break;
        }
        case "git-merge": {
          showStatus(`[${repoName}] Merging ${branch}...`);
          const mergeResult = await gitMerge(repo, branch);
          if (mergeResult.success) {
            showStatus(`[${repoName}] Merged ${branch}`);
          } else {
            showStatus(`[${repoName}] Merge conflicts detected`, true);
            showConflictsDialog(repo, branch);
          }
          break;
        }
        case "git-rebase":
          showStatus(`[${repoName}] Rebasing onto ${branch}...`);
          await gitRebase(repo, branch);
          showStatus(`[${repoName}] Rebased onto ${branch}`);
          break;
        case "git-push":
          showStatus(`[${repoName}] Pushing ${branch}...`);
          await gitPush(repo, branch);
          showStatus(`[${repoName}] Pushed ${branch}`);
          break;
        case "git-pull":
          if (gitContextBranch!.remote) {
            showStatus(`[${repoName}] Remote branch ${branch} is read-only; checkout a local branch to update it`, true);
            return;
          } else if (gitContextBranch!.upstream_gone) {
            const remote = remoteFromTracking(gitContextBranch!.tracking);
            showStatus(`[${repoName}] Pruning stale refs from ${remote}...`);
            await gitPrune(repo, remote);
            showStatus(`[${repoName}] ${branch}'s upstream no longer exists; set a new upstream or delete the local branch`, true);
          } else if (gitContextBranch!.current) {
            showStatus(`[${repoName}] Pulling ${branch}...`);
            await gitPull(repo);
            showStatus(`[${repoName}] Pulled ${branch}`);
          } else {
            if (!gitContextBranch!.tracking) {
              showStatus(`[${repoName}] ${branch} has no upstream; checkout or set upstream first`, true);
              return;
            }
            showStatus(`[${repoName}] Updating ${branch} from ${gitContextBranch!.tracking}...`);
            await gitPull(repo, branch, gitContextBranch!.tracking);
            showStatus(`[${repoName}] Updated ${branch}`);
          }
          break;
        case "git-pull-merge-master": {
          showStatus(`[${repoName}] Fetching master...`);
          await gitFetchBranch(repo, "origin", "master");
          showStatus(`[${repoName}] Merging origin/master...`);
          const pullMergeResult = await gitMerge(repo, "origin/master");
          if (pullMergeResult.success) {
            showStatus(`[${repoName}] Pulled and merged master`);
          } else {
            showStatus(`[${repoName}] Merge conflicts detected`, true);
            showConflictsDialog(repo, "origin/master");
          }
          break;
        }
        case "git-rename": {
          const newName = await appPrompt(`[${repoName}] Rename '${branch}' to:`, branch);
          if (newName && newName !== branch) {
            await gitRenameBranch(repo, branch, newName);
            showStatus(`[${repoName}] Renamed ${branch} to ${newName}`);
          }
          break;
        }
        case "git-delete":
          if (await appConfirm(`[${repoName}] Delete branch '${branch}'?`)) {
            await gitDeleteBranch(repo, branch, false);
            showStatus(`[${repoName}] Deleted ${branch}`);
          }
          break;
      }
      refreshSingleRepo(repo);
    } catch (err) {
      showStatus(`[${repoName}] Git: ${err}`, true);
      if (action === "git-pull") refreshSingleRepo(repo);
    }
  });

  document.getElementById("git-commit-context-menu")!.addEventListener("click", async (e) => {
    const action = (e.target as HTMLElement).dataset.action;
    if (!action || !gitContextCommit || !gitContextCommitRepo) return;
    hideGitCommitContextMenu();
    const commit = gitContextCommit;
    const repo = gitContextCommitRepo;
    const repoName = repo.split("/").pop();

    try {
      switch (action) {
        case "commit-copy-hash":
          await copyText(commit.hash);
          showStatus(`[${repoName}] Copied ${commit.short_hash}`);
          return;
        case "commit-copy-patch": {
          const patch = await gitCommitPatch(repo, commit.hash);
          await copyText(patch);
          showStatus(`[${repoName}] Copied patch for ${commit.short_hash}`);
          return;
        }
        case "commit-create-patch": {
          const defaultPath = `${repo}/${safePatchName(commit)}`;
          const target = await saveFileDialog(defaultPath);
          if (!target) return;
          const patch = await gitCommitPatch(repo, commit.hash);
          await writeFile(target, patch);
          showStatus(`[${repoName}] Patch saved`);
          return;
        }
        case "commit-cherry-pick":
          if (!(await appConfirm(`[${repoName}] Cherry-pick ${commit.short_hash}?`))) return;
          await gitCherryPick(repo, commit.hash);
          showStatus(`[${repoName}] Cherry-picked ${commit.short_hash}`);
          break;
        case "commit-checkout":
          if (!(await appConfirm(`[${repoName}] Checkout revision ${commit.short_hash}? This will detach HEAD.`))) return;
          await gitCheckoutRevision(repo, commit.hash);
          showStatus(`[${repoName}] Checked out ${commit.short_hash}`);
          break;
        case "commit-revert":
          if (!(await appConfirm(`[${repoName}] Revert commit ${commit.short_hash}?`))) return;
          await gitRevertCommit(repo, commit.hash);
          showStatus(`[${repoName}] Reverted ${commit.short_hash}`);
          break;
        case "commit-new-branch": {
          const name = await appPrompt(`[${repoName}] New branch from ${commit.short_hash}:`);
          if (!name) return;
          await gitNewBranchAtCommit(repo, name, commit.hash);
          showStatus(`[${repoName}] Created branch ${name}`);
          break;
        }
      }
      await refreshSingleRepo(repo, true);
      if (gitActiveLogRepo === repo && gitActiveLogBranch) showBranchLog(repo, gitActiveLogBranch);
    } catch (err) {
      showStatus(`[${repoName}] Commit action failed: ${err}`, true);
    }
  });

  document.addEventListener("click", () => {
    hideGitContextMenu();
    hideGitCommitContextMenu();
  });

  document.getElementById("git-refresh")!.addEventListener("click", () => {
    gitLoadedAt = 0;
    gitLoadedProject = "";
    gitRepoHeadCache.clear();
    gitRepoBranchCache.clear();
    gitActiveLogRepo = null;
    gitActiveLogBranch = null;
    gitActiveLogCommits = [];
    updateGitLogTitle(null, null);
    renderGitLogEmpty("Select a branch to view commits");
    loadGitBranches();
  });

  document.querySelector('[data-panel="git"]')!.addEventListener("click", () => {
    if (gitRepos.length === 0) {
      requestAnimationFrame(() => { loadGitBranches(); });
    }
  });
}
