// Multi-host descendant tree aggregation for Monocode Paseo Prototype.
// Builds a cycle-safe recursive tree (children -> grandchildren -> ...) across the
// local Paseo daemon and all online routed hosts, recognizing native Paseo parent
// labels (`paseo.parent-agent-id` + `paseo.parent-server-id`), `collab.by`,
// `parent_agent`, and same-workspace sibling anchors, plus cached latest-progress
// summaries fetched from tail timelines.
import {basename} from 'node:path';

const MAX_TREE_DEPTH = 8;
const MAX_TREE_NODES = 64;
const MAX_SUMMARY_FETCH = 24;
const SUMMARY_CONCURRENCY = 4;
const SUMMARY_TIMEOUT_MS = 4000;
const RUNNING_SUMMARY_TTL_MS = 12000;
const SUMMARY_CACHE_MAX = 256;
const HOST_CONNECT_TIMEOUT_MS = 8000;
const HOST_DIRECTORY_TIMEOUT_MS = 6000;
const MAX_SUMMARY_CHARS = 160;

const stamp = value => {
  const n = typeof value === 'number' ? value : Date.parse(value);
  return Number.isFinite(n) ? n : 0;
};

const nonEmpty = value => {
  if (typeof value !== 'string') return null;
  const trimmed = value.trim();
  return trimmed.length > 0 ? trimmed : null;
};

export function extractRepoName(cwd, projectName) {
  const raw = nonEmpty(cwd);
  if (raw) {
    const normalized = raw.replace(/\\/g, '/').replace(/\/+$/, '');
    const wt = normalized.match(/\/worktrees\/([^/]+)\/[^/]+$/);
    if (wt?.[1]) return wt[1];
  }
  const named = nonEmpty(projectName);
  if (named) return named;
  if (raw) return basename(raw.replace(/\/+$/, '')) || raw;
  return '';
}

function truncateText(text, maxChars = MAX_SUMMARY_CHARS) {
  if (!text) return '';
  const chars = [...text];
  if (chars.length <= maxChars) return text;
  return chars.slice(0, maxChars - 1).join('').trimEnd() + '…';
}

export function cleanAssistantSnippet(raw) {
  if (typeof raw !== 'string') return '';
  let text = raw
    .replace(/```[\s\S]*?```/g, ' [代码] ')
    .replace(/!\[([^\]]*)\]\([^)]+\)/g, (_, alt) => (alt?.trim() && alt.trim() !== 'Image' ? `[${alt.trim()}]` : '[图片]'))
    .replace(/\[([^\]]+)\]\([^)]+\)/g, '$1')
    .replace(/`([^`]+)`/g, '$1')
    .replace(/\*\*([^*]+)\*\*/g, '$1')
    .replace(/__([^_]+)__/g, '$1');
  const lines = text
    .split(/\r?\n/)
    .map(line => line.replace(/^\s*(?:#{1,6}\s+|[-*•]\s+|\d+\.\s+|>\s+)/, '').trim())
    .filter(line => line.length > 0 && !/^[-=|_]{3,}$/.test(line));
  if (!lines.length) return '';
  const joined = lines.slice(0, 3).join(' · ').replace(/\s+/g, ' ').trim();
  return truncateText(joined, MAX_SUMMARY_CHARS);
}

export function summarizeTailEntries(entries, agent) {
  const pending = agent?.pendingPermissions;
  if (Array.isArray(pending) && pending.length > 0) {
    const first = pending[0];
    const title = nonEmpty(first?.title) || nonEmpty(first?.name) || '等待权限确认';
    return {summaryKind: 'permission', summary: truncateText(`等待授权：${title}`, MAX_SUMMARY_CHARS)};
  }
  const list = Array.isArray(entries) ? entries : [];
  let latestAssistant = null;
  let latestError = null;
  let latestTool = null;
  let latestReasoning = null;
  for (let i = list.length - 1; i >= 0; i--) {
    const item = list[i]?.item ?? list[i];
    if (!item || typeof item !== 'object') continue;
    if (!latestAssistant && item.type === 'assistant_message') {
      const cleaned = cleanAssistantSnippet(item.text);
      if (cleaned) latestAssistant = cleaned;
    } else if (!latestError && item.type === 'error') {
      const msg = cleanAssistantSnippet(item.message ?? item.text ?? '');
      if (msg) latestError = msg;
    } else if (!latestTool && item.type === 'tool_call') {
      const label = nonEmpty(item.metadata?.title) || nonEmpty(item.name);
      if (label) latestTool = truncateText(label, MAX_SUMMARY_CHARS);
    } else if (!latestReasoning && item.type === 'reasoning') {
      const cleaned = cleanAssistantSnippet(item.text);
      if (cleaned) latestReasoning = cleaned;
    }
    if (latestAssistant && latestError && latestTool) break;
  }
  const isError = agent?.status === 'error';
  const isRunning = Boolean(agent?.activeTurn || agent?.status === 'running');
  if (isError && latestError) return {summaryKind: 'error', summary: latestError};
  if (latestAssistant) return {summaryKind: 'assistant', summary: latestAssistant};
  if (isRunning && latestTool) return {summaryKind: 'tool', summary: truncateText(`执行中：${latestTool}`, MAX_SUMMARY_CHARS)};
  if (latestError) return {summaryKind: 'error', summary: latestError};
  if (latestReasoning) return {summaryKind: 'reasoning', summary: latestReasoning};
  if (latestTool) return {summaryKind: 'tool', summary: latestTool};
  return {summaryKind: null, summary: null};
}

function deriveNodeStatus(entry) {
  const agent = entry.agent;
  if (agent.archivedAt) return 'archived';
  if (agent.pendingPermissions?.length) return 'attention';
  if (agent.status === 'error' || entry.paseoBucket === 'failed') return 'error';
  if (agent.activeTurn || agent.status === 'running' || entry.paseoBucket === 'running') return 'running';
  if (entry.paseoBucket === 'attention') return 'attention';
  return 'done';
}

function readableTitle(entry) {
  const a = entry.agent;
  const raw = nonEmpty(a.title);
  const uuidRe = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;
  const wsTitle = nonEmpty(entry.workspace?.title) || nonEmpty(entry.project?.workspaceName);
  const isAnchor = entry.paseoWorkspaceAnchorId === a.id;
  if (raw && !uuidRe.test(raw)) {
    const isChild = Boolean(a.labels?.['paseo.parent-agent-id'] || a.labels?.['collab.by'] || a.labels?.['parent_agent'] || !isAnchor);
    return isChild ? raw : (wsTitle || raw);
  }
  if (wsTitle) return wsTitle;
  for (const candidate of [a?.runtimeInfo?.extra?.title, a?.persistence?.metadata?.title]) {
    const text = nonEmpty(candidate);
    if (text && !uuidRe.test(text)) return text;
  }
  return raw || 'Paseo agent';
}

function agentKey(serverId, agentId) {
  return `${serverId}\0${agentId}`;
}

export function resolveParentKey(node, indexByKey, indexByAgentId) {
  const labels = node.entry.agent.labels ?? {};
  const selfKey = agentKey(node.serverId, node.agentId);

  const pickById = (targetAgentId, preferredServerId) => {
    if (!targetAgentId) return null;
    if (preferredServerId) {
      const exact = agentKey(preferredServerId, targetAgentId);
      if (exact !== selfKey && indexByKey.has(exact)) return exact;
    }
    const sameHost = agentKey(node.serverId, targetAgentId);
    if (sameHost !== selfKey && indexByKey.has(sameHost)) return sameHost;
    const candidates = (indexByAgentId.get(targetAgentId) ?? []).filter(k => k !== selfKey);
    if (candidates.length === 1) return candidates[0];
    if (candidates.length > 1) {
      const live = candidates.filter(k => !indexByKey.get(k)?.archived);
      if (live.length === 1) return live[0];
      return live[0] ?? candidates[0];
    }
    return null;
  };

  // 1. Native Paseo parent labels (supports cross-host via paseo.parent-server-id).
  const paseoParentId = nonEmpty(labels['paseo.parent-agent-id']);
  const paseoParentServer = nonEmpty(labels['paseo.parent-server-id']);
  if (paseoParentId) {
    return pickById(paseoParentId, paseoParentServer);
  }

  // 2. Cross-host collaboration label (`collab.by` from devbox-work-router).
  const collabBy = nonEmpty(labels['collab.by']);
  if (collabBy) {
    const collabServer = nonEmpty(labels['collab.server']) || paseoParentServer;
    return pickById(collabBy, collabServer);
  }

  // 3. Legacy `parent_agent` label.
  const parentAgent = nonEmpty(labels['parent_agent']);
  if (parentAgent) {
    return pickById(parentAgent, paseoParentServer);
  }

  // 4. Same-workspace sibling under workspace anchor (only when anchor is live/distinct).
  const anchorId = nonEmpty(node.entry.paseoWorkspaceAnchorId);
  if (node.entry.agent.workspaceId && anchorId && anchorId !== node.agentId) {
    const anchorKey = agentKey(node.serverId, anchorId);
    if (anchorKey !== selfKey && indexByKey.has(anchorKey)) return anchorKey;
  }
  return null;
}

function withTimeout(promise, ms, label) {
  let timer;
  const timeout = new Promise((_, reject) => {
    timer = setTimeout(() => reject(Error(`${label} timed out`)), ms);
    timer.unref?.();
  });
  return Promise.race([promise, timeout]).finally(() => clearTimeout(timer));
}

function nodeStatusRank(node) {
  if (node.archived) return 4;
  if (node.status === 'error') return 0;
  if (node.status === 'attention') return 1;
  if (node.status === 'running') return 2;
  return 3;
}

function toCompactRef(node) {
  return {
    serverId: node.serverId,
    hostLabel: node.hostLabel,
    isRemote: node.isRemote,
    agentId: node.agentId,
    title: node.title,
    cwd: node.cwd,
    repoName: node.repoName,
    status: node.status,
    archived: node.archived,
  };
}

export async function queryDescendants({
  host,
  registry,
  agentId,
  serverId,
  includeSummaries = true,
  summaryCache = new Map(),
}) {
  const rootAgentId = nonEmpty(agentId);
  if (!rootAgentId) throw Error('Missing agentId for paseo.descendants');
  const rootServerId = nonEmpty(serverId) || host.serverId;
  if (!rootServerId) throw Error('Missing serverId for paseo.descendants');

  const hosts = [];
  const unreachableHosts = [];
  const seenServers = new Set();

  const addHost = (entry) => {
    if (!entry?.host || !entry.serverId || seenServers.has(entry.serverId)) return;
    seenServers.add(entry.serverId);
    hosts.push(entry);
  };

  if (registry) {
    let dir = null;
    try {
      dir = await withTimeout(registry.list(), HOST_CONNECT_TIMEOUT_MS, 'hosts.list');
    } catch (error) {
      unreachableHosts.push({serverId: null, label: 'Connected hosts', error: error?.message || String(error)});
    }
    const primary = registry.primaryHost ?? (!host.isRemoteHost ? host : null);
    if (primary?.serverId) {
      addHost({
        serverId: primary.serverId,
        label: dir?.self?.label || 'Local Mac',
        isRemote: false,
        host: primary,
      });
    }
    if (dir?.supported && Array.isArray(dir.hosts)) {
      const onlineRemotes = dir.hosts.filter(h => h.serverId && h.enabled && h.status === 'online');
      await Promise.all(onlineRemotes.map(async h => {
        const existing = registry.routed.get(h.serverId);
        if (existing?.host?.connected) {
          addHost({serverId: h.serverId, label: h.label || existing.label || h.serverId, isRemote: true, host: existing.host});
          return;
        }
        try {
          await withTimeout(registry.connect(h.serverId), HOST_CONNECT_TIMEOUT_MS, `connect ${h.label || h.serverId}`);
          const active = registry.routed.get(h.serverId);
          if (active?.host) {
            addHost({serverId: h.serverId, label: h.label || active.label || h.serverId, isRemote: true, host: active.host});
          }
        } catch (error) {
          unreachableHosts.push({
            serverId: h.serverId,
            label: h.label || h.serverId,
            error: error?.message || String(error),
          });
        }
      }));
    }
  }

  // Always include the calling host itself even without a registry (e.g. unit tests).
  addHost({
    serverId: host.serverId,
    label: host.isRemoteHost ? host.serverId : 'Local Mac',
    isRemote: Boolean(host.isRemoteHost),
    host,
  });

  // Ensure each participating host has a fresh-enough in-memory directory.
  await Promise.all(hosts.map(async h => {
    try {
      await withTimeout(h.host.directory(false), HOST_DIRECTORY_TIMEOUT_MS, `directory ${h.label}`);
    } catch (error) {
      if (h.host.entries.size === 0) {
        unreachableHosts.push({serverId: h.serverId, label: h.label, error: error?.message || String(error)});
      }
    }
  }));

  // Build unified multi-host node index.
  const indexByKey = new Map();
  const indexByAgentId = new Map();
  for (const h of hosts) {
    for (const entry of h.host.entries.values()) {
      const a = entry.agent;
      const key = agentKey(h.serverId, a.id);
      const cwd = a.cwd || entry.project?.checkout?.cwd || '';
      const node = {
        key,
        serverId: h.serverId,
        hostLabel: h.label,
        isRemote: h.isRemote,
        host: h.host,
        entry,
        agentId: a.id,
        title: readableTitle(entry),
        cwd,
        repoName: extractRepoName(cwd, entry.project?.projectName),
        branch: entry.project?.checkout?.currentBranch ?? null,
        provider: a.provider || '',
        model: a.model || '',
        status: deriveNodeStatus(entry),
        needsInput: Boolean(a.pendingPermissions?.length),
        archived: Boolean(a.archivedAt),
        createdAt: stamp(a.createdAt),
        updatedAt: stamp(a.updatedAt),
        summary: null,
        summaryKind: null,
      };
      indexByKey.set(key, node);
      let list = indexByAgentId.get(a.id);
      if (!list) {
        list = [];
        indexByAgentId.set(a.id, list);
      }
      list.push(key);
    }
  }

  // Resolve parent -> children edges across all hosts.
  const parentOf = new Map();
  const childrenOf = new Map();
  for (const node of indexByKey.values()) {
    const pKey = resolveParentKey(node, indexByKey, indexByAgentId);
    if (!pKey) continue;
    parentOf.set(node.key, pKey);
    let kids = childrenOf.get(pKey);
    if (!kids) {
      kids = [];
      childrenOf.set(pKey, kids);
    }
    kids.push(node);
  }

  const rootKey = agentKey(rootServerId, rootAgentId);
  const rootNode = indexByKey.get(rootKey) ?? null;

  // Walk ancestors (parent -> grandparent -> ...) with cycle guard.
  const ancestors = [];
  const ancestorSeen = new Set([rootKey]);
  let cursor = parentOf.get(rootKey);
  while (cursor && !ancestorSeen.has(cursor) && ancestors.length < 5) {
    ancestorSeen.add(cursor);
    const anc = indexByKey.get(cursor);
    if (!anc) break;
    ancestors.push(toCompactRef(anc));
    cursor = parentOf.get(cursor);
  }
  const parent = ancestors[0] ?? null;

  // Walk descendants recursively with cycle guard and total node cap.
  const visited = new Set([rootKey]);
  const flatDescendants = [];
  const counts = {total: 0, active: 0, running: 0, attention: 0, error: 0, done: 0, archived: 0};

  const buildBranch = (parentKey, depth) => {
    if (depth > MAX_TREE_DEPTH || flatDescendants.length >= MAX_TREE_NODES) return [];
    const rawKids = (childrenOf.get(parentKey) ?? []).filter(k => !visited.has(k.key));
    rawKids.sort((a, b) =>
      Number(a.archived) - Number(b.archived)
      || (a.archived ? b.updatedAt - a.updatedAt : a.createdAt - b.createdAt)
      || nodeStatusRank(a) - nodeStatusRank(b)
      || a.agentId.localeCompare(b.agentId)
    );
    const out = [];
    for (const kid of rawKids) {
      if (flatDescendants.length >= MAX_TREE_NODES) break;
      visited.add(kid.key);
      flatDescendants.push(kid);
      counts.total++;
      if (kid.archived) counts.archived++;
      else {
        counts.active++;
        if (kid.status === 'running') counts.running++;
        else if (kid.status === 'attention') counts.attention++;
        else if (kid.status === 'error') counts.error++;
        else counts.done++;
      }
      const subChildren = buildBranch(kid.key, depth + 1);
      out.push({node: kid, children: subChildren});
    }
    return out;
  };

  const rawTree = buildBranch(rootKey, 1);

  // Fetch cached latest progress/conclusion summaries for active/recent descendants.
  if (includeSummaries && flatDescendants.length > 0) {
    const now = Date.now();
    const toFetch = [];
    const prioritized = [...flatDescendants].sort((a, b) =>
      Number(a.archived) - Number(b.archived) || b.updatedAt - a.updatedAt
    );
    for (const node of prioritized.slice(0, MAX_SUMMARY_FETCH)) {
      const cached = summaryCache.get(node.key);
      const fresh = cached
        && cached.updatedAt === node.updatedAt
        && cached.status === node.status
        && (node.status !== 'running' || now - cached.fetchedAt < RUNNING_SUMMARY_TTL_MS);
      if (fresh) {
        node.summary = cached.summary;
        node.summaryKind = cached.summaryKind;
        continue;
      }
      if (node.entry.agent.pendingPermissions?.length) {
        const s = summarizeTailEntries([], node.entry.agent);
        node.summary = s.summary;
        node.summaryKind = s.summaryKind;
        summaryCache.set(node.key, {updatedAt: node.updatedAt, status: node.status, ...s, fetchedAt: now});
        continue;
      }
      toFetch.push(node);
    }

    let cursorIdx = 0;
    const workers = Array.from({length: Math.min(SUMMARY_CONCURRENCY, toFetch.length)}, async () => {
      while (cursorIdx < toFetch.length) {
        const node = toFetch[cursorIdx++];
        if (typeof node.host.client?.fetchAgentTimeline !== 'function') continue;
        try {
          const page = await withTimeout(
            node.host.client.fetchAgentTimeline(node.agentId, {direction: 'tail', limit: 12, projection: 'projected'}),
            SUMMARY_TIMEOUT_MS,
            `timeline ${node.agentId}`,
          );
          const entries = page?.entries ?? [];
          const result = summarizeTailEntries(entries, node.entry.agent);
          node.summary = result.summary;
          node.summaryKind = result.summaryKind;
          summaryCache.delete(node.key);
          summaryCache.set(node.key, {updatedAt: node.updatedAt, status: node.status, ...result, fetchedAt: Date.now()});
          while (summaryCache.size > SUMMARY_CACHE_MAX) summaryCache.delete(summaryCache.keys().next().value);
        } catch {
          const fallback = summaryCache.get(node.key);
          if (fallback) {
            node.summary = fallback.summary;
            node.summaryKind = fallback.summaryKind;
          }
        }
      }
    });
    await Promise.all(workers);
  }

  const serialize = (branch) => branch.map(({node, children}) => ({
    serverId: node.serverId,
    hostLabel: node.hostLabel,
    isRemote: node.isRemote,
    agentId: node.agentId,
    title: node.title,
    cwd: node.cwd,
    repoName: node.repoName,
    branch: node.branch,
    provider: node.provider,
    model: node.model,
    status: node.status,
    needsInput: node.needsInput,
    archived: node.archived,
    createdAt: node.createdAt,
    updatedAt: node.updatedAt,
    summary: node.summary,
    summaryKind: node.summaryKind,
    children: serialize(children),
  }));

  return {
    root: {
      serverId: rootServerId,
      agentId: rootAgentId,
      hostLabel: rootNode?.hostLabel ?? hosts.find(h => h.serverId === rootServerId)?.label ?? '',
      title: rootNode?.title ?? null,
    },
    parent,
    ancestors,
    children: serialize(rawTree),
    counts,
    unreachableHosts,
  };
}
