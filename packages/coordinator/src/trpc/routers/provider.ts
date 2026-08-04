import { device, provider } from "@farm/db";
import { count, desc, eq } from "drizzle-orm";
import { adminProcedure, protectedProcedure, router } from "../init.ts";

export const providerRouter = router({
  list: protectedProcedure.query(({ ctx }) =>
    ctx.db
      .select({
        id: provider.id,
        name: provider.name,
        publicBaseUrl: provider.publicBaseUrl,
        hostname: provider.hostname,
        version: provider.version,
        status: provider.status,
        lastSeenAt: provider.lastSeenAt,
        deviceCount: count(device.id),
      })
      .from(provider)
      .leftJoin(device, eq(device.providerId, provider.id))
      .groupBy(provider.id)
      .orderBy(desc(provider.lastSeenAt)),
  ),

  // Token issuance and command dispatch arrive with the gateway in phase 2.
  _placeholder: adminProcedure.query(() => null),
});
