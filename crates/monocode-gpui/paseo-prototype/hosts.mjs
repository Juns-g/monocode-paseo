// Multi-host directory & per-host loopback servers.
// Host registrations come from the local Paseo daemon's hosts.json (read-only here). A remote host is linked either
// DIRECTLY by this sidecar (createHostClient over the saved relay offer / ws / ssh target, identity-pinned to the saved
// serverId) so it survives a stopped local daemon, or ROUTED via `origin.openHostRoute` through the local daemon.
// Each host is isolated in its own per-serverId loopback server and state directory. The saved `connection` secret is
// never logged or returned over RPC.
import {createHash,randomUUID,timingSafeEqual} from 'node:crypto';
import {mkdirSync,readFileSync,statSync} from 'node:fs';
import {join} from 'node:path';

export function hostStateDirectory(baseDirectory, serverId) {
  const digest = createHash('sha256').update(String(serverId)).digest('hex').slice(0, 16);
  return join(baseDirectory, 'hosts', digest);
}

// prototype.json `remoteHostTransport`: "auto" (default: direct when a saved connection exists, else routed) | "direct" | "routed".
export function hostTransport(value) {
  return ['auto', 'direct', 'routed'].includes(value) ? value : 'auto';
}

// Re-reads the daemon's hosts.json only when its mtime/size changes. null = no file (fall back to the origin directory).
export function savedHostsReader(path) {
  let cache = null;
  return () => {
    let stat;
    try {
      stat = statSync(path);
    } catch (error) {
      if (error.code === 'ENOENT') return null;
      return cache?.rows ?? null;
    }
    const key = `${stat.mtimeMs}:${stat.size}`;
    if (cache?.key === key) return cache.rows;
    try {
      const rows = JSON.parse(readFileSync(path, 'utf8'));
      if (!Array.isArray(rows)) throw Error('Invalid hosts.json');
      cache = {key, rows: rows.filter(row => row && typeof row === 'object' && typeof row.registrationId === 'string')};
    } catch {
      return cache?.rows ?? null; // A torn write keeps the previous directory.
    }
    return cache.rows;
  };
}

function withTimeout(promise, ms, message) {
  let timer;
  return Promise.race([
    promise,
    new Promise((_, reject) => {
      timer = setTimeout(() => reject(Error(message)), ms);
      timer.unref?.();
    }),
  ]).finally(() => clearTimeout(timer));
}

const FAILURE_STATUS_MS = 30000;
// SDK/socket errors may quote the target; the saved connection (pairing offer) must never leave this process.
export function redactConnection(message, connection) {
  let text = String(message ?? '');
  if (connection) text = text.split(connection).join('<connection>');
  return text.replace(/([#?&]offer=)[^\s'"&#]+/gi, '$1<redacted>');
}

export class HostRegistry {
  constructor({originClient, baseDirectory, token, createAdapter, createRoutedClient, createServer,
    readSavedHosts = null, createDirectClient = null, transport = 'auto', localServerId = null, directConnectTimeoutMs = 20000}) {
    if (!originClient) throw Error('Missing origin Paseo client');
    if (!baseDirectory) throw Error('Missing base directory');
    if (typeof createAdapter !== 'function') throw Error('Missing adapter factory');
    if (typeof createRoutedClient !== 'function') throw Error('Missing routed client factory');
    if (typeof createServer !== 'function') throw Error('Missing host server factory');
    this.origin = originClient;
    this.baseDirectory = baseDirectory;
    this.token = token; // Process-local loopback token; never exposed in RPC responses.
    this.createAdapter = createAdapter;
    this.createRoutedClient = createRoutedClient;
    this.createServer = createServer;
    this.routed = new Map(); // serverId -> { client, host, server, endpoint, label, unsubscribers, connecting }
    this.primaryHost = null;
    this.summaryCache = new Map();
    this.listCache = null; // { at, raw }
    this.listInFlight = null;
    this.readSavedHosts = readSavedHosts;
    this.createDirectClient = createDirectClient;
    this.transport = hostTransport(transport);
    this.localServerId = localServerId;
    this.directConnectTimeoutMs = directConnectTimeoutMs;
    this.failures = new Map(); // serverId -> {at, error}: last direct-connect failure, shown as offline for a while.
  }

  savedHosts() {
    return this.transport === 'routed' ? null : this.readSavedHosts?.() ?? null;
  }

  async originDirectory(force) {
    const now = Date.now();
    if (!force && this.listCache && now - this.listCache.at < 15000) return this.listCache.raw;
    if (!this.listInFlight) {
      this.listInFlight = Promise.resolve()
        .then(() => this.origin.listHosts())
        .then(raw => {
          this.listCache = {at: Date.now(), raw};
          return raw;
        })
        .finally(() => {
          this.listInFlight = null;
        });
    }
    return this.listInFlight;
  }

  liveStatus(serverId) {
    const active = serverId ? this.routed.get(serverId) : null;
    const connected = Boolean(active?.endpoint && active.host?.connected);
    return {active, connected};
  }

  verifyToken(value) {
    return typeof value === 'string'
      && typeof this.token === 'string'
      && this.token.length > 0
      && value.length === this.token.length
      && timingSafeEqual(Buffer.from(value), Buffer.from(this.token));
  }

  async list({force = false} = {}) {
    const info = this.origin.getLastServerInfoMessage?.();
    const fallbackSelf = {
      serverId: info?.serverId || this.localServerId || '',
      label: info?.hostname || 'Local Mac',
    };
    const originUp = info?.features?.connectedHosts === true;
    const saved = this.savedHosts();
    if (saved) {
      // Works with the origin daemon down: our own live link decides; the origin's view only fills unlinked hosts.
      const originRows = new Map();
      if (originUp) {
        try {
          for (const h of (await this.originDirectory(force))?.hosts ?? []) if (h.serverId) originRows.set(h.serverId, h);
        } catch {}
      }
      return {
        supported: true,
        self: fallbackSelf,
        hosts: saved.map(h => {
          const {active, connected} = this.liveStatus(h.serverId);
          const failure = h.serverId ? this.failures.get(h.serverId) : null;
          const recentFailure = failure && Date.now() - failure.at < FAILURE_STATUS_MS ? failure : null;
          const origin = h.serverId ? originRows.get(h.serverId) : null;
          const status = !h.enabled ? 'disabled'
            : active?.direct ? (connected ? 'online' : 'connecting')
            : connected ? 'online'
            : recentFailure ? 'offline'
            : origin?.status ?? h.status ?? 'offline';
          return {
            registrationId: h.registrationId,
            serverId: h.serverId ?? null,
            label: h.label || h.targetDescription || h.serverId || h.registrationId,
            enabled: Boolean(h.enabled),
            status,
            targetDescription: h.targetDescription ?? '',
            lastOnlineAt: origin?.lastOnlineAt ?? h.lastOnlineAt ?? null,
            error: recentFailure?.error ?? (connected ? null : origin?.error ?? null),
            connected,
            connectedEndpoint: connected ? active.endpoint : null,
            isRemote: true,
          };
        }),
      };
    }
    if (!originUp) {
      return {supported: false, self: fallbackSelf, hosts: []};
    }
    const dir = await this.originDirectory(force);
    const self = {
      serverId: dir?.self?.serverId || fallbackSelf.serverId,
      label: dir?.self?.label || fallbackSelf.label,
    };
    return {
      supported: true,
      self,
      hosts: (dir?.hosts ?? []).map(h => {
        const active = h.serverId ? this.routed.get(h.serverId) : null;
        const connected = Boolean(active?.endpoint && active.host?.connected);
        return {
          registrationId: h.registrationId,
          serverId: h.serverId ?? null,
          label: h.label || h.targetDescription || h.serverId || h.registrationId,
          enabled: Boolean(h.enabled),
          status: h.status,
          targetDescription: h.targetDescription ?? '',
          lastOnlineAt: h.lastOnlineAt ?? null,
          error: h.error ?? null,
          connected,
          connectedEndpoint: connected ? active.endpoint : null,
          isRemote: true,
        };
      }),
    };
  }

  async connect(input) {
    const serverId = typeof input === 'string'
      ? input.trim()
      : typeof input?.serverId === 'string'
        ? input.serverId.trim()
        : '';
    if (!serverId) {
      throw Error('Missing target host serverId');
    }
    const localId = this.origin.getLastServerInfoMessage?.()?.serverId || this.localServerId;
    if (localId && serverId === localId) {
      throw Error('Local host is already connected as primary');
    }

    const existing = this.routed.get(serverId);
    if (existing) {
      if (existing.connecting) {
        return existing.connecting;
      }
      // A direct link reconnects by itself: keep its endpoint instead of tearing it down.
      if (existing.endpoint && (existing.host?.connected || existing.direct)) {
        return {
          serverId,
          label: existing.label,
          status: existing.host?.connected ? 'online' : 'connecting',
          endpoint: existing.endpoint,
          version: existing.client?.getLastServerInfoMessage?.()?.version ?? null,
          isRemote: true,
        };
      }
      this.disposeRecord(serverId, existing);
    }

    const row = this.savedHosts()?.find(h => h.serverId === serverId) ?? null;
    if (row && !row.enabled) throw Error(`远程宿主机 ${row.label || serverId} 已停用`);
    const direct = this.transport !== 'routed' && typeof this.createDirectClient === 'function'
      && typeof row?.connection === 'string' && row.connection.length > 0;
    if (this.transport === 'direct' && !direct) throw Error('No direct connection saved for this Paseo host');

    const record = {
      direct,
      client: null,
      host: null,
      server: null,
      endpoint: null,
      label: serverId,
      unsubscribers: [],
      connecting: null,
    };
    this.routed.set(serverId, record);

    record.connecting = (async () => {
      let openedRouteId = null;
      let transportBound = false;
      try {
        let client;
        if (direct) {
          record.label = row.label || row.targetDescription || serverId;
        } else {
          const dir = await this.list();
          if (!dir.supported) {
            throw Error('Local Paseo daemon does not support connected hosts');
          }
          const summary = dir.hosts.find(h => h.serverId === serverId);
          if (!summary) {
            throw Error('Unknown Paseo remote host');
          }
          if (!summary.enabled || summary.status !== 'online') {
            throw Error(`远程宿主机 ${summary.label} 当前离线 (${summary.status})`);
          }
          record.label = summary.label;
        }

        const hostDir = hostStateDirectory(this.baseDirectory, serverId);
        mkdirSync(hostDir, {recursive: true, mode: 0o700});

        if (direct) {
          client = this.createDirectClient({connection: row.connection, serverId});
        } else {
          const scopeId = `monocode-paseo-routed-${randomUUID()}`;
          openedRouteId = await this.origin.openHostRoute(serverId, scopeId);
          client = this.createRoutedClient({
            origin: this.origin,
            serverId,
            scopeId,
            routeId: openedRouteId,
          });
        }
        record.client = client;
        const adapter = this.createAdapter(client, hostDir, {
          isRemoteHost: true,
          expectedServerId: serverId,
          descendantRegistry: this,
        });
        record.host = adapter;
        const store = {
          get environmentId() {
            return adapter.serverId;
          },
          authenticated: value => this.verifyToken(value),
          projects: () => [],
          project: () => {
            throw Error('Remote Paseo host filesystem is not mounted on this Mac');
          },
        };
        const server = this.createServer(store, adapter);
        record.server = server;

        transportBound = true;
        await (direct
          ? withTimeout(client.connect(), this.directConnectTimeoutMs, `远程宿主机 ${record.label} 直连超时`)
          : client.connect());
        const observed = client.getLastServerInfoMessage?.()?.serverId;
        if (observed !== serverId) {
          throw Error(`Remote Paseo host identity mismatch (expected ${serverId}, got ${observed ?? 'none'})`);
        }
        adapter.connected = true;
        client.setReconnectEnabled?.(true);
        const unsubStatus = client.subscribeConnectionStatus?.(state => {
          // Identity stays pinned across direct reconnects: adapter.identity() refuses a different serverId.
          adapter.connected = state.status === 'connected';
          adapter.invalidate();
        });
        if (typeof unsubStatus === 'function') record.unsubscribers.push(unsubStatus);
        const unsubRaw = client.subscribeRawMessages?.(msg => {
          if (msg?.type === 'agent_status') {
            if (typeof adapter.applyAgentStatus === 'function') adapter.applyAgentStatus(msg);
            else adapter.invalidate();
          }
        });
        if (typeof unsubRaw === 'function') record.unsubscribers.push(unsubRaw);

        // Warm the (windowed) directory without blocking connect; the first projects.list joins this in-flight fetch.
        adapter.directory(true).catch(() => {});
        await new Promise((resolve, reject) => {
          const onError = err => reject(err);
          server.once('error', onError);
          server.listen(0, '127.0.0.1', () => {
            server.off?.('error', onError);
            resolve();
          });
        });
        const addr = server.address();
        if (!addr || typeof addr !== 'object' || !addr.port) {
          throw Error('Failed to bind loopback server for remote Paseo host');
        }
        record.endpoint = `http://127.0.0.1:${addr.port}`;
        adapter.prewarmCatalog?.().catch(() => {});
        this.failures.delete(serverId);
        return {
          serverId,
          label: record.label,
          status: 'online',
          endpoint: record.endpoint,
          version: record.client?.getLastServerInfoMessage?.()?.version ?? null,
          isRemote: true,
          transport: direct ? 'direct' : 'routed',
        };
      } catch (error) {
        if (direct) {
          const message = redactConnection(error?.message || 'Direct host connection failed', row.connection);
          this.failures.set(serverId, {at: Date.now(), error: {code: 'host_offline', message}});
          if (error && typeof error === 'object' && error.message !== message) {
            try { error.message = message; } catch {}
            if (typeof error.stack === 'string') try { error.stack = redactConnection(error.stack, row.connection); } catch {}
          }
        }
        if (openedRouteId && !transportBound) {
          try {
            this.origin.closeHostRoute?.(openedRouteId);
          } catch {}
        }
        this.disposeRecord(serverId, record);
        throw error;
      } finally {
        record.connecting = null;
      }
    })();

    return record.connecting;
  }

  disposeRecord(serverId, record) {
    if (this.routed.get(serverId) === record) {
      this.routed.delete(serverId);
    }
    for (const unsub of record.unsubscribers ?? []) {
      try {
        unsub();
      } catch {}
    }
    if (record.host) {
      try {
        record.host.close();
      } catch {}
    } else if (record.client) {
      try {
        record.client.close();
      } catch {}
    }
    if (record.server) {
      try {
        record.server.close();
        record.server.closeAllConnections?.();
      } catch {}
    }
  }

  close() {
    for (const [serverId, record] of [...this.routed.entries()]) {
      this.disposeRecord(serverId, record);
    }
    this.routed.clear();
  }
}
