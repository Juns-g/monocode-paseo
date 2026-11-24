import { test } from 'node:test';
import assert from 'node:assert/strict';
import {
  normalizePosixPath,
  validateWorktreeSpec,
  listProjectWorktrees,
  verifyExistingWorktreeCwd,
} from './worktrees.mjs';

test('normalizePosixPath trims trailing slashes and preserves root', () => {
  assert.equal(normalizePosixPath('/repo/project///'), '/repo/project');
  assert.equal(normalizePosixPath('/'), '/');
  assert.equal(normalizePosixPath(''), '/');
});

test('validateWorktreeSpec accepts valid branch-off and checkout specs and rejects unsafe branch names', () => {
  assert.deepEqual(validateWorktreeSpec({ action: 'branch-off', branchName: 'feat/wt', baseBranch: 'main' }), {
    action: 'branch-off',
    branchName: 'feat/wt',
    baseBranch: 'main',
    refName: 'main',
  });
  assert.deepEqual(validateWorktreeSpec({ action: 'branch-off' }), {
    action: 'branch-off',
  });
  assert.deepEqual(validateWorktreeSpec({ action: 'checkout', refName: 'release/v1' }), {
    action: 'checkout',
    refName: 'release/v1',
  });

  for (const bad of [
    null,
    [],
    { action: 'unknown' },
    { action: 'branch-off', branchName: '../bad' },
    { action: 'branch-off', branchName: 'foo bar' },
    { action: 'branch-off', branchName: '/leading' },
    { action: 'branch-off', branchName: 'trailing/' },
    { action: 'branch-off', branchName: 'branch.lock' },
    { action: 'branch-off', baseBranch: 'bad..ref' },
    { action: 'checkout' },
  ]) {
    assert.throws(() => validateWorktreeSpec(bad));
  }
});

test('listProjectWorktrees merges checkout worktrees, paseo-owned worktrees, branches and workspace choices', async () => {
  const host = {
    projects: new Map([['paseo:/repo', { id: 'paseo:/repo', cwd: '/repo', name: 'Repo' }]]),
    entries: new Map([
      [
        'a1',
        {
          projectId: 'paseo:/repo',
          project: { checkout: { cwd: '/repo-wt/feat', currentBranch: 'feat/wt' } },
          workspace: {
            id: 'ws-wt',
            title: 'Feature worktree',
            workspaceKind: 'worktree',
            worktreeSlug: 'feat',
            workspaceDirectory: '/repo-wt/feat',
          },
          agent: { id: 'a1', workspaceId: 'ws-wt' },
        },
      ],
    ]),
    directory: async () => {},
    identity: () => {},
    workspaceChoices: () => [{ id: 'ws-wt', title: 'Feature worktree', cwd: '/repo-wt/feat' }],
    client: {
      listCheckoutWorktrees: async cwd => {
        assert.equal(cwd, '/repo');
        return [
          { path: '/repo', branchName: 'main', head: '1111111', createdAt: '2026-10-25T00:00:00Z', containsCwd: true },
          { path: '/repo-wt/feat/', branchName: 'feat/wt', head: '2222222', createdAt: '2026-10-26T00:00:00Z', containsCwd: false },
        ];
      },
      getPaseoWorktreeList: async ({ cwd }) => {
        assert.equal(cwd, '/repo');
        return { worktrees: [{ worktreePath: '/repo-wt/feat' }] };
      },
      getBranchSuggestions: async ({ cwd, query, limit }) => {
        assert.equal(cwd, '/repo');
        assert.equal(query, 'ma');
        assert.equal(limit, 30);
        return { branches: ['main', 'master'] };
      },
    },
  };

  const info = await listProjectWorktrees(host, { projectId: 'paseo:/repo', query: ' ma ' });
  assert.equal(info.projectId, 'paseo:/repo');
  assert.equal(info.isGitRepo, true);
  assert.equal(info.currentBranch, 'main');
  assert.deepEqual(info.branches, ['main', 'master']);
  assert.equal(info.worktrees.length, 2);
  assert.deepEqual(info.worktrees[0], {
    path: '/repo',
    branchName: 'main',
    head: '1111111',
    createdAt: '2026-10-25T00:00:00Z',
    containsCwd: true,
    isPaseoOwned: false,
    workspaceId: null,
    workspaceTitle: null,
  });
  assert.deepEqual(info.worktrees[1], {
    path: '/repo-wt/feat/',
    branchName: 'feat/wt',
    head: '2222222',
    createdAt: '2026-10-26T00:00:00Z',
    containsCwd: false,
    isPaseoOwned: true,
    workspaceId: 'ws-wt',
    workspaceTitle: 'Feature worktree',
  });
  assert.deepEqual(info.workspaces, [
    {
      id: 'ws-wt',
      title: 'Feature worktree',
      cwd: '/repo-wt/feat',
      branch: 'feat/wt',
      isWorktree: true,
    },
  ]);
});

test('listProjectWorktrees handles non-git directories gracefully and verifyExistingWorktreeCwd enforces membership', async () => {
  const host = {
    projects: new Map([['paseo:/plain', { id: 'paseo:/plain', cwd: '/plain', name: 'Plain' }]]),
    entries: new Map(),
    directory: async () => {},
    identity: () => {},
    comparePath: normalizePosixPath,
    workspaceChoices: () => [],
    client: {
      listCheckoutWorktrees: async () => {
        throw Error('Not a git repository');
      },
      getPaseoWorktreeList: async () => ({ worktrees: [], error: 'Not a git repository' }),
      getBranchSuggestions: async () => ({ branches: [], error: 'Not a git repository' }),
    },
  };

  const info = await listProjectWorktrees(host, { projectId: 'paseo:/plain' });
  assert.equal(info.isGitRepo, false);
  assert.equal(info.currentBranch, null);
  assert.deepEqual(info.worktrees, []);
  assert.deepEqual(info.branches, []);

  host.client.listCheckoutWorktrees = async () => [{ path: '/plain', branchName: 'main' }, { path: '/plain-wt' }];
  assert.equal(await verifyExistingWorktreeCwd(host, '/plain', '/plain-wt/'), '/plain-wt');
  await assert.rejects(
    verifyExistingWorktreeCwd(host, '/plain', '/outside/other'),
    /不属于该项目的 Git worktree/,
  );
});
