import { TRPCError } from "@trpc/server";
import type { CommandData, CommandPayload, CoordinatorMessage, PreloadInfo } from "@yard/protocol";

export const COMMAND_TIMEOUT_MS = 15_000;

interface Pending {
  resolve: (data: CommandData | undefined) => void;
  reject: (err: Error) => void;
  timer: ReturnType<typeof setTimeout>;
}

/**
 * One live control-plane socket.
 *
 * `send` is injected rather than holding the WebSocket directly so the state
 * machine can be tested against a plain function.
 */
export class ProviderConnection {
  readonly providerId: string;
  readonly publicBaseUrl: string;
  private readonly pending = new Map<string, Pending>();
  private preloads: PreloadInfo[];
  private closed = false;

  constructor(
    providerId: string,
    publicBaseUrl: string,
    private readonly send: (msg: CoordinatorMessage) => void,
    preloads: PreloadInfo[] = [],
  ) {
    this.providerId = providerId;
    this.publicBaseUrl = publicBaseUrl;
    this.preloads = preloads;
  }

  /** The provider's current protected-preload desired state. */
  get preloadedApps(): readonly PreloadInfo[] {
    return this.preloads;
  }

  /** Replaces the whole snapshot received during provider registration. */
  setPreloads(preloads: PreloadInfo[]) {
    this.preloads = preloads;
  }

  /** Applies a new or replaced preload reported after the initial snapshot. */
  upsertPreload(preload: PreloadInfo) {
    const index = this.preloads.findIndex(
      (existing) => existing.deviceId === preload.deviceId && existing.appId === preload.appId,
    );
    if (index === -1) {
      this.preloads = [...this.preloads, preload];
    } else {
      this.preloads = this.preloads.with(index, preload);
    }
  }

  /** Drops one preload after the provider has removed its policy and app. */
  removePreload(deviceId: string, appId: string) {
    this.preloads = this.preloads.filter(
      (preload) => preload.deviceId !== deviceId || preload.appId !== appId,
    );
  }

  push(msg: CoordinatorMessage) {
    if (this.closed) return;
    this.send(msg);
  }

  /**
   * Sends a command and waits for its `command.result`.
   *
   * Every command is correlated by id and bounded by a timeout — a provider
   * that accepts a command and never answers must not leak a pending promise
   * or wedge the caller's request forever.
   */
  command(payload: CommandPayload): Promise<CommandData | undefined> {
    if (this.closed) {
      return Promise.reject(new Error(`provider ${this.providerId} is disconnected`));
    }
    const id = crypto.randomUUID();

    return new Promise<CommandData | undefined>((resolve, reject) => {
      const timer = setTimeout(() => {
        this.pending.delete(id);
        reject(new Error(`command ${payload.kind} timed out after ${COMMAND_TIMEOUT_MS}ms`));
      }, COMMAND_TIMEOUT_MS);

      this.pending.set(id, { resolve, reject, timer });
      this.send({ type: "command", id, payload });
    });
  }

  /** Fire-and-forget: for pushes whose failure must not fail the caller. */
  commandNoWait(payload: CommandPayload) {
    this.command(payload).catch((err) => {
      console.warn(`[gateway] ${this.providerId}: ${payload.kind} failed:`, err.message);
    });
  }

  settle(id: string, ok: boolean, data: CommandData | undefined, error?: string) {
    const entry = this.pending.get(id);
    if (!entry) return; // already timed out, or a duplicate result
    clearTimeout(entry.timer);
    this.pending.delete(id);
    if (ok) entry.resolve(data);
    else entry.reject(new Error(error ?? "command failed"));
  }

  close() {
    this.closed = true;
    for (const [, entry] of this.pending) {
      clearTimeout(entry.timer);
      entry.reject(new Error(`provider ${this.providerId} disconnected`));
    }
    this.pending.clear();
  }

  get isClosed() {
    return this.closed;
  }
}

/**
 * Live provider sockets, by provider id.
 *
 * In-memory and therefore per-coordinator-instance. Running more than one
 * coordinator would need this moved behind Postgres LISTEN/NOTIFY or a bus —
 * see docs/OPERATIONS.md. Providers dial out, so a second instance is not a
 * transparent scale-out.
 */
class ProviderRegistry {
  private readonly connections = new Map<string, ProviderConnection>();

  add(conn: ProviderConnection) {
    // A reconnect before the old socket's close handler runs would otherwise
    // leave the stale entry winning.
    this.connections.get(conn.providerId)?.close();
    this.connections.set(conn.providerId, conn);
  }

  remove(conn: ProviderConnection) {
    if (this.connections.get(conn.providerId) === conn) {
      this.connections.delete(conn.providerId);
    }
    conn.close();
  }

  get(providerId: string): ProviderConnection | undefined {
    return this.connections.get(providerId);
  }

  /** Throws the tRPC error a router should surface when a provider is gone. */
  require(providerId: string): ProviderConnection {
    const conn = this.connections.get(providerId);
    if (!conn || conn.isClosed) {
      throw new TRPCError({
        code: "PRECONDITION_FAILED",
        message: "The provider owning this device is offline",
      });
    }
    return conn;
  }

  get onlineIds(): string[] {
    return [...this.connections.keys()];
  }

  /** Current preload metadata from every connected provider. */
  get preloadedApps(): readonly PreloadInfo[] {
    return [...this.connections.values()].flatMap((conn) => [...conn.preloadedApps]);
  }
}

export const providers = new ProviderRegistry();
