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
