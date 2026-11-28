import {test} from 'node:test';
import assert from 'node:assert/strict';
import {createServer as createHttpServer} from 'node:http';
import {existsSync,mkdtempSync,rmSync} from 'node:fs';
import {tmpdir} from 'node:os';
import {join} from 'node:path';
import {HostRegistry,hostStateDirectory} from './hosts.mjs';

const TOKEN = 'A'.repeat(43);

function makeOrigin({
  serverId = 'local-mac-server',
  hostname = 'MacBook-Pro',
  connectedHosts = true,
  hosts = [],
} = {}) {
  let routeCounter = 0;
  const closedRoutes = [];
  const openedRoutes = [];
  return {
    closedRoutes,
    openedRoutes,
    getLastServerInfoMessage: () => ({
      serverId,
      hostname,
      version: '0.17.4',
      features: {connectedHosts},
    }),
    listHosts: async () => ({
      self: {serverId, label: hostname},
      hosts,
    }),
    openHostRoute: async (targetServerId, scopeId) => {
      const routeId = `route-${++routeCounter}`;
      openedRoutes.push({targetServerId, scopeId, routeId});
      return routeId;
    },
    closeHostRoute: routeId => {
      closedRoutes.push(routeId);
    },
  };
}

function makeFakeHttpServer(store, adapter) {
  return createHttpServer(async (req, res) => {
    const auth = req.headers.authorization?.match(/^Bearer (.+)$/)?.[1] ?? '';
    if (!store.authenticated(auth)) {
      res.writeHead(401).end(JSON.stringify({error: 'unauthorized'}));
      return;
    }
    const chunks = [];
    for await (const c of req) chunks.push(c);
    const body = JSON.parse(Buffer.concat(chunks).toString('utf8'));
    const out = await adapter.request(body.method, body.params ?? {});
    res.writeHead(200, {'Content-Type': 'application/json'}).end(JSON.stringify(out));
  });
}

test('HostRegistry.list returns supported:false when connectedHosts feature is absent', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'paseo-hosts-test-'));
  try {
    const origin = makeOrigin({connectedHosts: false});
    const registry = new HostRegistry({
      originClient: origin,
      baseDirectory: dir,
      token: TOKEN,
      createAdapter: () => ({}),
      createRoutedClient: () => ({}),
      createServer: makeFakeHttpServer,
    });
    const res = await registry.list();
    assert.deepEqual(res, {
      supported: false,
      self: {serverId: 'local-mac-server', label: 'MacBook-Pro'},
      hosts: [],
    });
    await assert.rejects(registry.connect('devbox-boe'), /does not support connected hosts/);
  } finally {
    rmSync(dir, {recursive: true, force: true});
  }
});

test('HostRegistry.connect opens one route, binds authenticated loopback server, caches repeat calls, and isolates state dirs', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'paseo-hosts-test-'));
  const origin = makeOrigin({
    hosts: [
      {
        registrationId: 'reg-boe',
        serverId: 'srv-devbox-boe',
        label: 'devbox-boe',
        enabled: true,
        status: 'online',
        targetDescription: 'ssh://devbox-boe',
        lastOnlineAt: '2026-11-24T00:00:00Z',
        error: null,
      },
      {
        registrationId: 'reg-ai',
        serverId: 'srv-devbox-ai',
        label: 'devbox-ai',
        enabled: true,
        status: 'offline',
        targetDescription: 'ssh://devbox-ai',
        lastOnlineAt: null,
        error: {code: 'offline', message: 'Host unreachable'},
      },
    ],
  });

  const adapterDirs = new Map();
  let statusHandler = null;
  const registry = new HostRegistry({
    originClient: origin,
    baseDirectory: dir,
    token: TOKEN,
    createRoutedClient: ({serverId}) => ({
      connect: async () => {},
      close: () => {},
      setReconnectEnabled: () => {},
      getLastServerInfoMessage: () => ({serverId, version: '0.17.4-remote'}),
      subscribeConnectionStatus: cb => {
        statusHandler = cb;
        return () => {};
      },
      subscribeRawMessages: () => () => {},
    }),
    createAdapter: (_client, hostDir, options) => {
      adapterDirs.set(options.expectedServerId, hostDir);
      return {
        serverId: options.expectedServerId,
        isRemoteHost: options.isRemoteHost,
        connected: false,
        invalidate() {},
        async directory() {},
        async request(method) {
          return {result: {method, serverId: options.expectedServerId}};
        },
        close() {},
      };
    },
    createServer: makeFakeHttpServer,
  });

  try {
    const before = await registry.list();
    assert.equal(before.supported, true);
    assert.equal(before.hosts.length, 2);
    assert.equal(before.hosts[0].connected, false);
    assert.equal(before.hosts[0].connectedEndpoint, null);

    // Concurrent connect calls share one route & one server.
    const [first, second] = await Promise.all([
      registry.connect('srv-devbox-boe'),
      registry.connect({serverId: 'srv-devbox-boe'}),
    ]);
    assert.deepEqual(first, second);
    assert.equal(origin.openedRoutes.length, 1);
    assert.match(first.endpoint, /^http:\/\/127\.0\.0\.1:\d+$/);
    assert.equal(first.version, '0.17.4-remote');
    assert.equal(first.token, undefined, 'token must never leak over RPC');

    const boeDir = hostStateDirectory(dir, 'srv-devbox-boe');
    const aiDir = hostStateDirectory(dir, 'srv-devbox-ai');
    assert.notEqual(boeDir, aiDir);
    assert.equal(adapterDirs.get('srv-devbox-boe'), boeDir);
    assert.equal(existsSync(boeDir), true);

    // Loopback endpoint requires exact token.
    const unauth = await fetch(`${first.endpoint}/rpc`, {
      method: 'POST',
      headers: {Authorization: 'Bearer wrong'},
      body: JSON.stringify({method: 'environment.describe'}),
    });
    assert.equal(unauth.status, 401);

    const auth = await fetch(`${first.endpoint}/rpc`, {
      method: 'POST',
      headers: {Authorization: `Bearer ${TOKEN}`},
      body: JSON.stringify({method: 'environment.describe'}),
    });
    assert.equal(auth.status, 200);
    assert.deepEqual(await auth.json(), {
      result: {method: 'environment.describe', serverId: 'srv-devbox-boe'},
    });

    // Offline, local, missing, and unknown hosts reject cleanly.
    await assert.rejects(registry.connect('srv-devbox-ai'), /离线/);
    await assert.rejects(registry.connect('local-mac-server'), /already connected as primary/);
    await assert.rejects(registry.connect('unknown-host'), /Unknown Paseo remote host/);
    await assert.rejects(registry.connect(''), /Missing target host serverId/);

    // Simulated disconnect marks host disconnected and allows clean re-connect later.
    statusHandler({status: 'disconnected'});
    const mid = await registry.list();
    assert.equal(mid.hosts[0].connected, false);
    const reconnected = await registry.connect('srv-devbox-boe');
    assert.equal(origin.openedRoutes.length, 2);
    assert.match(reconnected.endpoint, /^http:\/\/127\.0\.0\.1:\d+$/);
  } finally {
    registry.close();
    rmSync(dir, {recursive: true, force: true});
  }
});

test('HostRegistry.connect aborts and tears down route on remote serverId mismatch', async () => {
  const dir = mkdtempSync(join(tmpdir(), 'paseo-hosts-mismatch-'));
  const origin = makeOrigin({
    hosts: [
      {
        registrationId: 'reg-boe',
        serverId: 'srv-devbox-boe',
        label: 'devbox-boe',
        enabled: true,
        status: 'online',
        targetDescription: 'ssh://devbox-boe',
        lastOnlineAt: null,
        error: null,
      },
    ],
  });
  let adapterClosed = false;
  let clientClosed = false;
  const registry = new HostRegistry({
    originClient: origin,
    baseDirectory: dir,
    token: TOKEN,
    createRoutedClient: () => ({
      connect: async () => {},
      close: () => {
        clientClosed = true;
      },
      getLastServerInfoMessage: () => ({serverId: 'srv-impostor', version: '0.17.4'}),
    }),
    createAdapter: client => ({
      serverId: 'srv-devbox-boe',
      connected: false,
      invalidate() {},
      async directory() {},
      close() {
        adapterClosed = true;
        client.close();
      },
    }),
    createServer: makeFakeHttpServer,
  });

  try {
    await assert.rejects(registry.connect('srv-devbox-boe'), /identity mismatch/);
    assert.equal(adapterClosed, true);
    assert.equal(clientClosed, true);
    assert.equal(registry.routed.size, 0);
  } finally {
    registry.close();
    rmSync(dir, {recursive: true, force: true});
  }
});

test('direct transport: hosts.json directory and identity-pinned direct links work with the origin daemon down and never expose the saved connection', async () => {
  const {writeFileSync} = await import('node:fs');
  const {savedHostsReader, hostTransport} = await import('./hosts.mjs');
  const dir = mkdtempSync(join(tmpdir(), 'paseo-hosts-direct-'));
  const SECRET = 'https://app.paseo.sh/#offer=SECRET-OFFER';
  const hostsPath = join(dir, 'hosts.json');
  const row = (serverId, label, extra = {}) => ({registrationId: `hst-${label}`, serverId, label, enabled: true, status: 'online',
    targetDescription: `relay://relay.paseo.sh:443/${serverId}`, lastOnlineAt: '2026-11-28T00:00:00Z', nextRetryAt: null, error: null,
    connection: `${SECRET}-${label}`, ...extra});
  writeFileSync(hostsPath, JSON.stringify([row('srv-byte', 'Byte'), row('srv-impostor', 'Impostor')]), {mode: 0o600});
  // Origin daemon is down: no hello, every origin RPC fails.
  const origin = {
    openedRoutes: [],
    getLastServerInfoMessage: () => null,
    listHosts: async () => { throw Error('origin down'); },
    openHostRoute: async () => { origin.openedRoutes.push(1); throw Error('origin down'); },
  };
  const made = [];
  const registry = new HostRegistry({
    originClient: origin, baseDirectory: dir, token: TOKEN, localServerId: 'srv-local',
    readSavedHosts: savedHostsReader(hostsPath),
    createDirectClient: ({connection, serverId}) => {
      const client = {connection, closed: false, status: null,
        connect: async () => {}, close() { client.closed = true; }, setReconnectEnabled() {},
        getLastServerInfoMessage: () => ({serverId: serverId === 'srv-impostor' ? 'srv-someone-else' : serverId, version: '0.18.0'}),
        subscribeConnectionStatus: cb => { client.status = cb; return () => {}; },
        subscribeRawMessages: () => () => {}};
      made.push(client);
      return client;
    },
    createRoutedClient: () => { throw Error('routed path must not be used'); },
    createAdapter: (client, _hostDir, options) => ({serverId: options.expectedServerId, isRemoteHost: true, connected: false,
      invalidate() {}, async directory() {}, async request(method) { return {result: {method}}; }, close() { client.close(); }}),
    createServer: makeFakeHttpServer,
  });
  try {
    assert.equal(hostTransport(undefined), 'auto');assert.equal(hostTransport('direct'), 'direct');assert.equal(hostTransport('bogus'), 'auto');
    const before = await registry.list();
    assert.equal(before.supported, true);
    assert.equal(before.self.serverId, 'srv-local');
    assert.deepEqual(before.hosts.map(h => [h.serverId, h.status, h.connected]), [['srv-byte', 'online', false], ['srv-impostor', 'online', false]]);
    assert.equal(JSON.stringify(before).includes('SECRET'), false, 'connection never listed');
    assert.equal('connection' in before.hosts[0], false);

    const linked = await registry.connect('srv-byte');
    assert.match(linked.endpoint, /^http:\/\/127\.0\.0\.1:\d+$/);
    assert.equal(linked.transport, 'direct');
    assert.equal(JSON.stringify(linked).includes('SECRET'), false);
    assert.equal(made[0].connection, `${SECRET}-Byte`);
    assert.equal(origin.openedRoutes.length, 0, 'no origin route');
    let listed = await registry.list();
    assert.deepEqual([listed.hosts[0].status, listed.hosts[0].connected, listed.hosts[0].connectedEndpoint], ['online', true, linked.endpoint]);

    // A dropped direct link reconnects by itself: shown as connecting, endpoint kept, no second client.
    made[0].status({status: 'connecting'});
    listed = await registry.list();
    assert.deepEqual([listed.hosts[0].status, listed.hosts[0].connected], ['connecting', false]);
    const again = await registry.connect('srv-byte');
    assert.equal(again.endpoint, linked.endpoint);assert.equal(again.status, 'connecting');assert.equal(made.length, 1);
    made[0].status({status: 'connected'});

    // Identity pin: a saved connection that reaches another daemon is refused and torn down.
    await assert.rejects(registry.connect('srv-impostor'), /identity mismatch/);
    assert.equal(made[1].closed, true);
    listed = await registry.list();
    assert.equal(listed.hosts[1].status, 'offline');assert.equal(listed.hosts[1].connected, false);
    assert.equal(registry.routed.has('srv-impostor'), false);
    assert.equal(JSON.stringify(listed).includes('SECRET'), false);

    // hosts.json is re-read when it changes (mtime/size).
    writeFileSync(hostsPath, JSON.stringify([row('srv-byte', 'Byte'), row('srv-new', 'Newbox', {enabled: false})]), {mode: 0o600});
    listed = await registry.list();
    assert.deepEqual(listed.hosts.map(h => [h.serverId, h.status]), [['srv-byte', 'online'], ['srv-new', 'disabled']]);
  } finally {
    registry.close();
    rmSync(dir, {recursive: true, force: true});
  }
});

test('direct transport: connect errors that quote the saved connection are redacted before list/RPC output', async () => {
  const {writeFileSync} = await import('node:fs');
  const {savedHostsReader, redactConnection} = await import('./hosts.mjs');
  const dir = mkdtempSync(join(tmpdir(), 'paseo-hosts-redact-'));
  const connection = 'https://app.paseo.sh/#offer=LEAKYSECRET';
  const hostsPath = join(dir, 'hosts.json');
  writeFileSync(hostsPath, JSON.stringify([{registrationId: 'hst-l', serverId: 'srv-leaky', label: 'Leaky', enabled: true, status: 'online', connection}]));
  const registry = new HostRegistry({
    originClient: {getLastServerInfoMessage: () => null, listHosts: async () => { throw Error('down'); }},
    baseDirectory: dir, token: TOKEN, localServerId: 'srv-local', readSavedHosts: savedHostsReader(hostsPath),
    createDirectClient: () => ({connect: async () => { throw Error(`Invalid URL: ${connection}`); }, close() {},
      getLastServerInfoMessage: () => null}),
    createRoutedClient: () => { throw Error('routed path must not be used'); },
    createAdapter: (client) => ({serverId: 'srv-leaky', isRemoteHost: true, invalidate() {}, async directory() {}, close() { client.close(); }}),
    createServer: makeFakeHttpServer,
  });
  try {
    assert.equal(redactConnection('x wss://relay/?offer=abc&y', null), 'x wss://relay/?offer=<redacted>&y');
    const error = await registry.connect('srv-leaky').catch(e => e);
    assert.equal(error.message, 'Invalid URL: <connection>');
    assert.equal(String(error.stack).includes('LEAKYSECRET'), false);
    const listed = await registry.list();
    assert.equal(listed.hosts[0].status, 'offline');
    assert.equal(JSON.stringify(listed).includes('LEAKYSECRET'), false);
  } finally {
    registry.close();
    rmSync(dir, {recursive: true, force: true});
  }
});
