import type { Database } from "@farm/db";
import { auditLog } from "@farm/db";

/** Fire-and-shape an audit row. Never throws into the caller's happy path. */
export async function audit(
  db: Database,
  actorUserId: string | null,
  action: string,
  targetType?: string,
  targetId?: string,
  metadata?: Record<string, unknown>,
) {
  try {
    await db.insert(auditLog).values({
      id: crypto.randomUUID(),
      actorUserId,
      action,
      targetType,
      targetId,
      metadata,
    });
  } catch (err) {
    console.error("[audit] failed to record", action, err);
  }
}
