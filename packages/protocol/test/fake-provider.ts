/**
 * A provider that speaks the control plane but owns no hardware.
 *
 * This is what lets the coordinator and the entire web UI be built and
 * regression-tested before any Rust exists — and afterwards, it stays the
 * fastest way to reproduce inventory and reservation behaviour without
 * plugging in a phone.
 *
 * As a library, for tests:
 *     const fake = new FakeProvider({ url, token, providerId, devices });
 *     await fake.connect();
 *
 * As a CLI, to populate a running dev coordinator:
 *     bun packages/protocol/test/fake-provider.ts --token pft_… --devices 4
 */
import {
  type CommandData,
  type CommandPayload,
  CoordinatorMessage,
  type DeviceSnapshot,
  PROTOCOL_VERSION,
  type ProviderMessage,
} from "../src/index.ts";

export interface FakeProviderOptions {
  /** Coordinator base URL, e.g. http://localhost:3000 */
  url: string;
  token: string;
  providerId: string;
  name?: string;
  publicBaseUrl?: string;
  devices: DeviceSnapshot[];
  /** Override how a command is answered. Return data, or throw to fail it. */
  onCommand?: (
    payload: CommandPayload,
  ) => CommandData | undefined | Promise<CommandData | undefined>;
  verbose?: boolean;
}

export class FakeProvider {
  private ws: WebSocket | null = null;
  private heartbeat: ReturnType<typeof setInterval> | null = null;
  private nextAdbPort = 15000;

  /** Every command the coordinator has sent, in order. Assertions read this. */
  readonly received: CommandPayload[] = [];
  /** Resolves once `hello.ack` arrives. */
  readonly ready: Promise<void>;
  private markReady!: () => void;
  private failReady!: (err: Error) => void;

  constructor(private readonly opts: FakeProviderOptions) {
    this.ready = new Promise((resolve, reject) => {
      this.markReady = resolve;
      this.failReady = reject;
    });
  }

  async connect(): Promise<void> {
    const url = new URL("/api/providers/connect", this.opts.url);
    url.protocol = url.protocol === "https:" ? "wss:" : "ws:";

    // Bun's WebSocket accepts headers; browsers do not. Providers are servers,
    // so a bearer header is the right shape here.
    this.ws = new WebSocket(url, {
      headers: { Authorization: `Bearer ${this.opts.token}` },
    } as unknown as string[]);

    this.ws.addEventListener("open", () => {
      this.send({
        type: "hello",
        protocolVersion: PROTOCOL_VERSION,
        providerId: this.opts.providerId,
        name: this.opts.name ?? this.opts.providerId,
        version: "fake-0.1.0",
        publicBaseUrl: this.opts.publicBaseUrl ?? "https://fake-provider.test",
        hostname: "fake.local",
        devices: this.opts.devices,
      });
    });

    this.ws.addEventListener("message", (event) => {
      void this.onMessage(String(event.data));
    });

    this.ws.addEventListener("error", () => {
      this.failReady(new Error("fake provider socket error (is the token valid?)"));
    });

    this.ws.addEventListener("close", (event) => {
      this.log(`closed: ${event.code} ${event.reason}`);
      if (this.heartbeat) clearInterval(this.heartbeat);
      this.failReady(new Error(`socket closed before hello.ack: ${event.reason || event.code}`));
    });

    await this.ready;
  }

  private async onMessage(raw: string) {
    const msg = CoordinatorMessage.parse(JSON.parse(raw));

    switch (msg.type) {
      case "hello.ack":
        this.log(`connected — heartbeat every ${msg.heartbeatIntervalMs}ms`);
        this.heartbeat = setInterval(
          () => this.send({ type: "heartbeat", at: Date.now() }),
          msg.heartbeatIntervalMs,
        );
        this.markReady();
        break;

      case "hello.reject":
        this.failReady(new Error(`rejected: ${msg.reason}`));
        break;

      case "command": {
        this.received.push(msg.payload);
        this.log(`command ${msg.payload.kind}`);
        try {
          const data = await this.answer(msg.payload);
          this.send({ type: "command.result", id: msg.id, ok: true, data });
        } catch (err) {
          this.send({
            type: "command.result",
            id: msg.id,
            ok: false,
            error: err instanceof Error ? err.message : String(err),
          });
        }
        break;
      }

      case "ping":
        break;
    }
  }

  /** Plausible default answers, so tests only override what they care about. */
  private async answer(payload: CommandPayload): Promise<CommandData | undefined> {
    if (this.opts.onCommand) return await this.opts.onCommand(payload);

    switch (payload.kind) {
      case "device.apps":
        return {
          apps: [
            { id: "com.example.app", name: "Example", version: "1.0.0" },
            { id: "com.android.settings", name: "Settings", system: true },
          ],
        };
      case "device.adb.expose":
        return { adbPort: this.nextAdbPort++ };
      default:
        return undefined;
    }
  }

  /** Pushes a new state for one device, as real hotplug would. */
  setDeviceStatus(deviceId: string, status: DeviceSnapshot["status"], note?: string) {
    this.send({ type: "device.status", deviceId, status, note });
  }

  upsertDevice(snapshot: DeviceSnapshot) {
    this.send({ type: "device.upsert", device: snapshot });
  }

  removeDevice(deviceId: string) {
    this.send({ type: "device.removed", deviceId });
  }

  private send(msg: ProviderMessage) {
    this.ws?.send(JSON.stringify(msg));
  }

  private log(line: string) {
    if (this.opts.verbose) console.log(`[fake-provider ${this.opts.providerId}] ${line}`);
  }

  async close() {
    if (this.heartbeat) clearInterval(this.heartbeat);
    this.ws?.close(1000, "bye");
    this.ws = null;
    // Give the coordinator's close handler time to land before a test asserts
    // on the database.
    await new Promise((r) => setTimeout(r, 150));
  }
}

/** Synthetic devices with realistic identifiers, models and geometry. */
export function makeDevices(count: number): DeviceSnapshot[] {
  const templates: Array<Omit<DeviceSnapshot, "id" | "status">> = [
    {
      platform: "ios",
      name: "iPhone 15 Pro",
      model: "iPhone16,1",
      manufacturer: "Apple",
      osVersion: "17.4.1",
      abi: "arm64e",
      display: { width: 1179, height: 2556, scale: 3, rotation: 0 },
      battery: { level: 0.82, state: "discharging" },
      streamCodec: "hev1.1.6.L93.B0",
    },
    {
      platform: "android",
      name: "Pixel 8",
      model: "Pixel 8",
      manufacturer: "Google",
      osVersion: "14",
      abi: "arm64-v8a",
      sdk: 34,
      display: { width: 1080, height: 2400, scale: 2.625, rotation: 0 },
      battery: { level: 0.64, state: "charging" },
      streamCodec: "avc1.640028",
    },
    {
      platform: "android",
      name: "Galaxy S23",
      model: "SM-S911B",
      manufacturer: "Samsung",
      osVersion: "14",
      abi: "arm64-v8a",
      sdk: 34,
      display: { width: 1080, height: 2340, scale: 2.625, rotation: 0 },
      battery: { level: 0.91, state: "full" },
      streamCodec: "avc1.640028",
    },
    {
      platform: "ios",
      name: 'iPad Pro 11"',
      model: "iPad14,3",
      manufacturer: "Apple",
      osVersion: "17.4",
      abi: "arm64e",
      display: { width: 1668, height: 2388, scale: 2, rotation: 0 },
      battery: { level: 0.55, state: "discharging" },
      streamCodec: "hev1.1.6.L93.B0",
    },
  ];

  return Array.from({ length: count }, (_, i) => {
    const t = templates[i % templates.length]!;
    const suffix = String(i + 1).padStart(2, "0");
    return {
      ...t,
      id: t.platform === "ios" ? `00008120-000C1D2E3F4012${suffix}` : `R5CT10ABC${suffix}`,
      name: `${t.name} #${i + 1}`,
      status: "ready" as const,
    };
  });
}

// ── CLI ────────────────────────────────────────────────────────────────────

if (import.meta.main) {
  const args = new Map<string, string>();
  for (let i = 2; i < Bun.argv.length; i += 2) {
    const key = Bun.argv[i]?.replace(/^--/, "");
    if (key) args.set(key, Bun.argv[i + 1] ?? "");
  }

  const token = args.get("token") ?? process.env.FARM_PROVIDER_TOKEN;
  if (!token) {
    console.error(
      "usage: bun packages/protocol/test/fake-provider.ts --token <pft_…> " +
        "[--url http://localhost:3000] [--id fake-1] [--devices 4]\n\n" +
        "Create a provider and token under /admin/providers first.",
    );
    process.exit(1);
  }

  const fake = new FakeProvider({
    url: args.get("url") ?? "http://localhost:3000",
    token,
    providerId: args.get("id") ?? "fake-1",
    name: args.get("name") ?? "Fake provider",
    publicBaseUrl: args.get("publicBaseUrl") ?? "https://fake-provider.test",
    devices: makeDevices(Number(args.get("devices") ?? 4)),
    verbose: true,
  });

  await fake.connect();
  console.log("[fake-provider] running — ctrl-c to stop");

  for (const signal of ["SIGINT", "SIGTERM"] as const) {
    process.on(signal, async () => {
      await fake.close();
      process.exit(0);
    });
  }
}
