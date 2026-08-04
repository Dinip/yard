/**
 * Fan-out for "something about the device inventory changed".
 *
 * Deliberately carries **no payload**. Subscribers re-read the device list
 * rather than applying a delta, which is the same reconcile-don't-patch stance
 * the control plane takes: a missed or reordered notification can never leave a
 * client permanently disagreeing with the database.
 *
 * In-memory, so it only reaches clients attached to this coordinator instance.
 * Multi-instance would move this to Postgres LISTEN/NOTIFY.
 */
type Listener = () => void;

class Broadcast {
  private readonly listeners = new Set<Listener>();
  private scheduled: ReturnType<typeof setTimeout> | null = null;

  subscribe(listener: Listener): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  /**
   * Coalesces bursts. A provider reconnecting emits one event per device; the
   * UI only needs to know that *something* changed, and a 50ms window turns a
   * 40-device reconnect into a single refetch.
   */
  publish() {
    if (this.scheduled) return;
    this.scheduled = setTimeout(() => {
      this.scheduled = null;
      for (const listener of this.listeners) {
        try {
          listener();
        } catch (err) {
          console.error("[events] listener threw:", err);
        }
      }
    }, 50);
  }

  get listenerCount() {
    return this.listeners.size;
  }
}

export const deviceEvents = new Broadcast();
