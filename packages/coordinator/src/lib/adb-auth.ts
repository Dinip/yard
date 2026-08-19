import { deviceEvents } from "./events.ts";

/**
 * `adb connect` attempts waiting on a holder's answer.
 *
 * **In memory, not a table.** A request is bound to a live TCP connection
 * parked on a provider. If this process restarts, the provider has already
 * refused that connection, so a persisted row could only ever describe
 * something nobody can approve. The durable trace is the `auditLog` row written
 * when somebody answers.
 *
 * This is the opposite call from `joinRequest`, which *is* a table: there the
 * requester's browser is what waits, and it survives a socket blip.
 */

/**
 * How long a request stays answerable.
 *
 * Matches `ADB_AUTH_TIMEOUT` in `provider-core/src/adb_auth.rs`. The provider's
 * clock is the one that counts — it owns the parked socket — so this is
 * deliberately not longer: an entry outliving the connection it describes would
 * put an Approve button on screen that does nothing.
 */
export const ADB_AUTH_TTL = 120_000;

export interface PendingAdbAuth {
  requestId: string;
  deviceId: string;
  providerId: string;
  fingerprint: string;
  publicKey: string;
  comment?: string;
  askedAt: Date;
  expiresAt: Date;
}

class AdbAuthRequests {
  private readonly pending = new Map<string, PendingAdbAuth>();

  add(entry: Omit<PendingAdbAuth, "askedAt" | "expiresAt">) {
    const askedAt = new Date();
    this.pending.set(entry.requestId, {
      ...entry,
      askedAt,
      expiresAt: new Date(askedAt.getTime() + ADB_AUTH_TTL),
    });

    // Nothing sweeps this map on a schedule: each entry retires itself, and a
    // coordinator with no `adb connect` traffic should not be running a timer.
    const timer = setTimeout(() => {
      if (this.pending.delete(entry.requestId)) deviceEvents.publish();
    }, ADB_AUTH_TTL);
    timer.unref?.();

    deviceEvents.publish();
  }

  /** Take a request out of the map, so two approvals cannot both answer it. */
  claim(requestId: string): PendingAdbAuth | undefined {
    const found = this.pending.get(requestId);
    if (found) this.pending.delete(requestId);
    return found;
  }

  /** What the device page shows the holder, oldest first. */
  forDevice(deviceId: string): PendingAdbAuth[] {
    return [...this.pending.values()]
      .filter((entry) => entry.deviceId === deviceId)
      .sort((a, b) => a.askedAt.getTime() - b.askedAt.getTime());
  }

  /**
   * Forget everything parked on a provider that just went away.
   *
   * Its sockets went with it, so every one of these is already refused.
   */
  dropProvider(providerId: string) {
    let removed = false;
    for (const [id, entry] of this.pending) {
      if (entry.providerId === providerId) {
        this.pending.delete(id);
        removed = true;
      }
    }
    if (removed) deviceEvents.publish();
  }

  /** Same, for a device whose session ended. */
  dropDevices(deviceIds: string[]) {
    const ids = new Set(deviceIds);
    let removed = false;
    for (const [id, entry] of this.pending) {
      if (ids.has(entry.deviceId)) {
        this.pending.delete(id);
        removed = true;
      }
    }
    if (removed) deviceEvents.publish();
  }
}

export const adbAuthRequests = new AdbAuthRequests();
