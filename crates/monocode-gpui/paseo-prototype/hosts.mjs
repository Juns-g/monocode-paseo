// Multi-host directory & routed loopback servers over the local Paseo daemon.
// MonoCode never touches SSH keys, relays, or remote credentials directly; every
// remote host (`devbox-boe`, `devbox-ai`, ...) is tunneled via `origin.openHostRoute`
// and isolated in its own per-serverId loopback server and state directory.
import {createHash,randomUUID,timingSafeEqual} from 'node:crypto';
import {mkdirSync} from 'node:fs';
import {join} from 'node:path';

export function hostStateDirectory(baseDirectory, serverId) {
  const digest = createHash('sha256').update(String(serverId)).digest('hex').slice(0, 16);
  return join(baseDirectory, 'hosts', digest);
}

export class HostRegistry {
  constructor({originClient, baseDirectory, token, createAdapter, createRoutedClient, createServer}) {
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
      serverId: info?.serverId ?? '',
      label: info?.hostname || 'Local Mac',
    };
    if (info?.features?.connectedHosts !== true) {
      return {supported: false, self: fallbackSelf, hosts: []};
    }
    const now = Date.now();
    let dir;
    if (!force && this.listCache && now - this.listCache.at < 15000) {
      dir = this.listCache.raw;
    } else {
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
      dir = await this.listInFlight;
    }
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
    const localId = this.origin.getLastServerInfoMessage?.()?.serverId;
    if (localId && serverId === localId) {
      throw Error('Local host is already connected as primary');
    }

    const existing = this.routed.get(serverId);
    if (existing) {
      if (existing.connecting) {
        return existing.connecting;
      }
      if (existing.endpoint && existing.host?.connected) {
        return {
          serverId,
          label: existing.label,
          status: 'online',
          endpoint: existing.endpoint,
          version: existing.client?.getLastServerInfoMessage?.()?.version ?? null,
          isRemote: true,
        };
      }
      this.disposeRecord(serverId, existing);
    }

    const record = {
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

        const hostDir = hostStateDirectory(this.baseDirectory, serverId);
        mkdirSync(hostDir, {recursive: true, mode: 0o700});

        const scopeId = `monocode-paseo-routed-${randomUUID()}`;
        openedRouteId = await this.origin.openHostRoute(serverId, scopeId);
        const client = this.createRoutedClient({
          origin: this.origin,
          serverId,
          scopeId,
          routeId: openedRouteId,
        });
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
        await client.connect();
        const observed = client.getLastServerInfoMessage?.()?.serverId;
        if (observed !== serverId) {
          throw Error(`Remote Paseo host identity mismatch (expected ${serverId}, got ${observed ?? 'none'})`);
        }
        adapter.connected = true;
        client.setReconnectEnabled?.(true);
        const unsubStatus = client.subscribeConnectionStatus?.(state => {
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

        await adapter.directory(true);
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
        return {
          serverId,
          label: record.label,
          status: 'online',
          endpoint: record.endpoint,
          version: record.client?.getLastServerInfoMessage?.()?.version ?? null,
          isRemote: true,
        };
      } catch (error) {
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
