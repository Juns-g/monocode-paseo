import { isBusy } from './controller.mjs';

export const REWIND_MODES = ['conversation', 'files', 'both'];

export function supportedRewindModes(agent, serverInfo) {
  if (serverInfo?.features?.rewind === false) return [];
  const caps = agent?.capabilities;
  if (!caps || typeof caps !== 'object') return [];
  const modes = [];
  if (caps.supportsRewindConversation) modes.push('conversation');
  if (caps.supportsRewindFiles) modes.push('files');
  if (caps.supportsRewindBoth) modes.push('both');
  return modes;
}

export function extractUserMessageId(entry) {
  if (typeof entry?.providerMessageId === 'string' && entry.providerMessageId.trim()) {
    return entry.providerMessageId;
  }
  const id = entry?.item?.messageId;
  return typeof id === 'string' && id.trim() ? id : undefined;
}

export async function rewindPaseoAgent(host, params) {
  const { commandId, sessionId, messageId, mode } = params ?? {};
  if (typeof commandId !== 'string' || !commandId.trim()) {
    throw Error('Missing command identity');
  }
  if (typeof sessionId !== 'string' || !sessionId.trim()) {
    throw Error('Missing Paseo session identity');
  }
  if (typeof messageId !== 'string' || !messageId.trim()) {
    throw Error('该消息没有可用的 Paseo messageId，无法回退。');
  }
  if (!REWIND_MODES.includes(mode)) {
    throw Error('不支持的回退模式。');
  }

  return host.attempts.run(
    { type: 'paseo.rewind', commandId, sessionId, messageId, mode },
    async () => {
      host.identity();
      const entry = await host.target(sessionId);
      const agent = entry.agent;
      if (agent.archivedAt) {
        throw Error('已归档的 Paseo 会话不支持回退。');
      }
      // Strict idle-only guard: never let daemon auto-cancel an active turn.
      if (isBusy(agent)) {
        throw Error('Paseo 会话正在运行或等待审批，请等待空闲后再回退。');
      }
      const allowed = supportedRewindModes(agent, host.client.getLastServerInfoMessage());
      if (!allowed.includes(mode)) {
        throw Error(`当前 Paseo 智能体不支持此回退模式 (${mode})。`);
      }

      // Re-verify daemon identity immediately prior to mutation.
      host.identity();
      await host.client.rewindAgent(agent.id, messageId, mode);

      // Force full epoch re-sync on next sessions.sync (drop cached entries/page/previous/snapshot).
      const cached = host.histories.get(agent.id);
      if (cached) {
        cached.entries = [];
        cached.page = null;
        cached.previous = null;
        cached.snapshot = null;
        cached.signature = null;
        cached.changedRows = null;
        cached.dirty = true;
        cached.generation = (cached.generation ?? 0) + 1;
      }
      host.invalidate();
      return {
        ok: true,
        commandId,
        sessionId: agent.id,
        messageId,
        mode,
        revision: ++host.revision,
      };
    },
  );
}
