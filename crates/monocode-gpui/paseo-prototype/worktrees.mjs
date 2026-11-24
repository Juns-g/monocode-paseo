const MAX_BRANCH_SUGGESTIONS = 30;
const BRANCH_RE = /^(?!\/)(?!.*\.\.)(?!.*[\s~^:?*\[\\])(?!.*\/$)(?!.*\.lock$).{1,128}$/;

export function normalizePosixPath(value) {
  return String(value ?? '').replace(/\/+$/, '') || '/';
}

export function validateWorktreeSpec(spec) {
  if (!spec || typeof spec !== 'object' || Array.isArray(spec)) {
    throw Error('Invalid Paseo worktree specification');
  }
  const action = spec.action ?? 'branch-off';
  if (!['branch-off', 'checkout'].includes(action)) {
    throw Error('Unsupported Paseo worktree action');
  }
  const branchName = typeof spec.branchName === 'string' && spec.branchName.trim()
    ? spec.branchName.trim() : undefined;
  const baseBranch = typeof spec.baseBranch === 'string' && spec.baseBranch.trim()
    ? spec.baseBranch.trim() : undefined;
  const refName = typeof spec.refName === 'string' && spec.refName.trim()
    ? spec.refName.trim() : undefined;

  if (branchName && !BRANCH_RE.test(branchName)) {
    throw Error('分支名称格式无效，请检查后重试。');
  }
  if (baseBranch && !BRANCH_RE.test(baseBranch)) {
    throw Error('基线分支名称格式无效，请检查后重试。');
  }
  if (refName && !BRANCH_RE.test(refName)) {
    throw Error('分支或引用名称格式无效，请检查后重试。');
  }
  if (action === 'checkout' && !refName) {
    throw Error('检出已有分支模式需要指定分支名。');
  }
  return {
    action,
    ...(branchName ? { branchName } : {}),
    ...(baseBranch ? { baseBranch } : {}),
    ...(action === 'checkout'
      ? { refName }
      : baseBranch ? { refName: baseBranch } : refName ? { refName } : {}),
  };
}

export async function listProjectWorktrees(host, params) {
  await host.directory();
  const project = host.projects.get(params?.projectId);
  if (!project) throw Error('Unknown Paseo project');

  const cwd = project.cwd;
  const query = typeof params?.query === 'string' ? params.query.trim() : undefined;

  const [checkoutRes, paseoRes, branchRes] = await Promise.allSettled([
    Promise.resolve().then(() =>
      typeof host.client.listCheckoutWorktrees === 'function'
        ? host.client.listCheckoutWorktrees(cwd)
        : [],
    ),
    Promise.resolve().then(() =>
      typeof host.client.getPaseoWorktreeList === 'function'
        ? host.client.getPaseoWorktreeList({ cwd })
        : { worktrees: [] },
    ),
    Promise.resolve().then(() =>
      typeof host.client.getBranchSuggestions === 'function'
        ? host.client.getBranchSuggestions({
            cwd,
            ...(query ? { query } : {}),
            limit: MAX_BRANCH_SUGGESTIONS,
          })
        : { branches: [] },
    ),
  ]);
  host.identity();

  const checkoutWorktrees = checkoutRes.status === 'fulfilled' && Array.isArray(checkoutRes.value)
    ? checkoutRes.value : [];
  const paseoOwnedSet = new Set(
    paseoRes.status === 'fulfilled' && Array.isArray(paseoRes.value?.worktrees)
      ? paseoRes.value.worktrees.map(w => normalizePosixPath(w.worktreePath))
      : [],
  );
  const branches = branchRes.status === 'fulfilled' && Array.isArray(branchRes.value?.branches)
    ? branchRes.value.branches : [];

  const workspaces = host.workspaceChoices(project.id).map(w => {
    const entry = [...host.entries.values()].find(
      e => e.projectId === project.id && e.workspace?.id === w.id,
    );
    const ws = entry?.workspace;
    return {
      ...w,
      branch: entry?.project?.checkout?.currentBranch ?? null,
      isWorktree: ws?.workspaceKind === 'worktree' || Boolean(ws?.worktreeSlug),
    };
  });
  const workspaceByPath = new Map(workspaces.map(w => [normalizePosixPath(w.cwd), w]));
  const projectNorm = normalizePosixPath(cwd);

  const worktrees = checkoutWorktrees.map(w => {
    const norm = normalizePosixPath(w.path);
    const linkedWorkspace = workspaceByPath.get(norm);
    return {
      path: w.path,
      branchName: w.branchName ?? null,
      head: w.head ?? null,
      createdAt: w.createdAt ?? null,
      containsCwd: Boolean(w.containsCwd) || norm === projectNorm,
      isPaseoOwned: paseoOwnedSet.has(norm),
      workspaceId: linkedWorkspace?.id ?? null,
      workspaceTitle: linkedWorkspace?.title ?? null,
    };
  });

  const currentBranch = checkoutWorktrees.find(w => w.containsCwd || normalizePosixPath(w.path) === projectNorm)?.branchName
    ?? [...host.entries.values()].find(e => e.projectId === project.id)?.project?.checkout?.currentBranch
    ?? null;

  return {
    projectId: project.id,
    isGitRepo: checkoutWorktrees.length > 0 || branches.length > 0,
    currentBranch,
    worktrees,
    branches,
    workspaces,
  };
}

export async function verifyExistingWorktreeCwd(host, projectCwd, requestedCwd) {
  const worktrees = typeof host.client.listCheckoutWorktrees === 'function'
    ? await host.client.listCheckoutWorktrees(projectCwd)
    : [];
  host.identity();
  const target = host.comparePath(requestedCwd);
  const matched = (Array.isArray(worktrees) ? worktrees : []).find(
    w => typeof w?.path === 'string' && host.comparePath(w.path) === target,
  );
  if (!matched) {
    throw Error('工作目录不属于该项目的 Git worktree；不会创建 worktree。');
  }
  return matched.path;
}
