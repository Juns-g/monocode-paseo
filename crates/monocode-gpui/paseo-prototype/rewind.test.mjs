import { test } from 'node:test';
import assert from 'node:assert/strict';
import { boundedEntries } from './controller.mjs';
import {
  REWIND_MODES,
  supportedRewindModes,
  extractUserMessageId,
  rewindPaseoAgent,
} from './rewind.mjs';

test('supportedRewindModes honors server feature gate and per-agent capability flags', () => {
  assert.deepEqual(REWIND_MODES, ['conversation', 'files', 'both']);
  assert.deepEqual(supportedRewindModes(null, {}), []);
  assert.deepEqual(supportedRewindModes({ capabilities: null }, {}), []);
  const allCaps = {
    capabilities: {
      supportsRewindConversation: true,
      supportsRewindFiles: true,
      supportsRewindBoth: true,
    },
  };
  assert.deepEqual(supportedRewindModes(allCaps, {}), ['conversation', 'files', 'both']);
  assert.deepEqual(supportedRewindModes(allCaps, { features: { rewind: true } }), [
    'conversation',
    'files',
    'both',
  ]);
  assert.deepEqual(supportedRewindModes(allCaps, { features: { rewind: false } }), []);
  assert.deepEqual(
    supportedRewindModes(
      {
        capabilities: {
          supportsRewindConversation: true,
          supportsRewindFiles: false,
          supportsRewindBoth: false,
        },
      },
      {},
    ),
    ['conversation'],
  );
});

test('extractUserMessageId prefers providerMessageId and boundedEntries preserves oversized user message IDs', () => {
  assert.equal(
    extractUserMessageId({
      providerMessageId: 'prov-1',
      item: { type: 'user_message', text: 'hi', messageId: 'client-1' },
    }),
    'prov-1',
  );
  assert.equal(
    extractUserMessageId({
      providerMessageId: '   ',
      item: { type: 'user_message', text: 'hi', messageId: 'msg-2' },
    }),
    'msg-2',
  );
  assert.equal(
    extractUserMessageId({
      item: { type: 'user_message', text: 'hi', clientMessageId: 'only-client' },
    }),
    undefined,
  );
  assert.equal(extractUserMessageId({ item: { type: 'user_message', text: 'hi', messageId: '' } }), undefined);

  const huge = 'x'.repeat(40000);
  const [bounded] = boundedEntries([
    {
      seqStart: 1,
      seqEnd: 1,
      providerMessageId: 'prov-oversized',
      item: {
        type: 'user_message',
        text: huge,
        messageId: 'msg-oversized',
        clientMessageId: 'client-oversized',
      },
    },
  ]);
  assert.equal(bounded.prototypeTruncated, true);
  assert.equal(bounded.item.messageId, 'msg-oversized');
  assert.equal(bounded.item.clientMessageId, 'client-oversized');
  assert.equal(extractUserMessageId(bounded), 'prov-oversized');

  const [boundedFallback] = boundedEntries([
    {
      seqStart: 2,
      seqEnd: 2,
      item: {
        type: 'user_message',
        text: huge,
        messageId: 'msg-fallback',
        clientMessageId: 'client-fallback',
      },
    },
  ]);
  assert.equal(extractUserMessageId(boundedFallback), 'msg-fallback');
});

test('rewindPaseoAgent validates parameters before entering command journal', async () => {
  let journaled = 0;
  const host = {
    attempts: {
      run: async (_cmd, fn) => {
        journaled++;
        return fn();
      },
    },
  };
  await assert.rejects(
    rewindPaseoAgent(host, { commandId: '', sessionId: 'a', messageId: 'm', mode: 'conversation' }),
    /Missing command identity/,
  );
  await assert.rejects(
    rewindPaseoAgent(host, { commandId: 'c1', sessionId: ' ', messageId: 'm', mode: 'conversation' }),
    /Missing Paseo session identity/,
  );
  await assert.rejects(
    rewindPaseoAgent(host, { commandId: 'c1', sessionId: 'a', messageId: '', mode: 'conversation' }),
    /messageId/,
  );
  await assert.rejects(
    rewindPaseoAgent(host, { commandId: 'c1', sessionId: 'a', messageId: 'm', mode: 'invalid' }),
    /不支持的回退模式/,
  );
  assert.equal(journaled, 0);
});
