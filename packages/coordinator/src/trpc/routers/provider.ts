import { TRPCError } from "@trpc/server";
import { device, provider, providerToken, user } from "@yard/db";
import { count, desc, eq } from "drizzle-orm";
import { z } from "zod";
import { providers } from "../../gateway/registry.ts";
import { audit } from "../../lib/audit.ts";
import { generateProviderToken } from "../../lib/provider-token.ts";
import { adminProcedure, protectedProcedure, router } from "../init.ts";

export const providerRouter = router({
  list: protectedProcedure.query(async ({ ctx }) => {
    const rows = await ctx.db
      .select({
        id: provider.id,
        name: provider.name,
        publicBaseUrl: provider.publicBaseUrl,
        hostname: provider.hostname,
        version: provider.version,
        status: provider.status,
        lastSeenAt: provider.lastSeenAt,
        createdAt: provider.createdAt,
        deviceCount: count(device.id),
      })
      .from(provider)
      .leftJoin(device, eq(device.providerId, provider.id))
      .groupBy(provider.id)
      .orderBy(desc(provider.lastSeenAt));

    // The `status` column is what the last socket event wrote; the registry is
    // ground truth for *right now*. They diverge if the process restarted while
    // providers were connected, and the live view is the one worth showing.
    const online = new Set(providers.onlineIds);
    return rows.map((r) => ({ ...r, connected: online.has(r.id) }));
  }),

  /**
   * Registers a provider before it has ever connected. The provider is created
   * here, not on first connect, because its token must exist for it to connect
   * at all — and an admin has to choose the publicBaseUrl the browser will use.
   */
  create: adminProcedure
    .input(
      z.object({
        id: z
          .string()
          .min(1)
          .max(64)
          .regex(/^[a-z0-9][a-z0-9-]*$/, "lowercase letters, digits and dashes only"),
        name: z.string().min(1).max(128),
        publicBaseUrl: z.string().url(),
      }),
    )
    .mutation(async ({ ctx, input }) => {
      const [existing] = await ctx.db
        .select({ id: provider.id })
        .from(provider)
        .where(eq(provider.id, input.id))
        .limit(1);
      if (existing)
        throw new TRPCError({ code: "CONFLICT", message: "Provider id already exists" });

      const [created] = await ctx.db
        .insert(provider)
        .values({
          id: input.id,
          name: input.name,
          // Trailing slashes make every URL join downstream ambiguous.
          publicBaseUrl: input.publicBaseUrl.replace(/\/+$/, ""),
          status: "offline",
        })
        .returning();

      await audit(ctx.db, ctx.user.id, "provider.create", "provider", input.id, {
        name: input.name,
      });
      return created!;
    }),

  update: adminProcedure
    .input(
      z.object({
        id: z.string(),
        name: z.string().min(1).max(128).optional(),
        publicBaseUrl: z.string().url().optional(),
      }),
    )
    .mutation(async ({ ctx, input }) => {
      const [updated] = await ctx.db
        .update(provider)
        .set({
          ...(input.name ? { name: input.name } : {}),
          ...(input.publicBaseUrl
            ? { publicBaseUrl: input.publicBaseUrl.replace(/\/+$/, "") }
            : {}),
          updatedAt: new Date(),
        })
        .where(eq(provider.id, input.id))
        .returning();
      if (!updated) throw new TRPCError({ code: "NOT_FOUND" });
      await audit(ctx.db, ctx.user.id, "provider.update", "provider", input.id);
      return updated;
    }),

  remove: adminProcedure.input(z.object({ id: z.string() })).mutation(async ({ ctx, input }) => {
    providers.get(input.id)?.push({ type: "hello.reject", reason: "provider deleted" });
    // Devices and tokens cascade; an active socket will fail on its next write.
    await ctx.db.delete(provider).where(eq(provider.id, input.id));
    await audit(ctx.db, ctx.user.id, "provider.delete", "provider", input.id);
    return { ok: true };
  }),

  tokens: router({
    list: adminProcedure.input(z.object({ providerId: z.string() })).query(({ ctx, input }) =>
      ctx.db
        .select({
          id: providerToken.id,
          label: providerToken.label,
          createdAt: providerToken.createdAt,
          lastUsedAt: providerToken.lastUsedAt,
          revokedAt: providerToken.revokedAt,
          createdByName: user.name,
        })
        .from(providerToken)
        .leftJoin(user, eq(providerToken.createdBy, user.id))
        .where(eq(providerToken.providerId, input.providerId))
        .orderBy(desc(providerToken.createdAt)),
    ),

    /** The plaintext is returned **once** and never stored — only its sha256 is. */
    create: adminProcedure
      .input(z.object({ providerId: z.string(), label: z.string().max(128).optional() }))
      .mutation(async ({ ctx, input }) => {
        const [target] = await ctx.db
          .select({ id: provider.id })
          .from(provider)
          .where(eq(provider.id, input.providerId))
          .limit(1);
        if (!target) throw new TRPCError({ code: "NOT_FOUND", message: "Unknown provider" });

        const { plaintext, hash } = generateProviderToken();
        const [created] = await ctx.db
          .insert(providerToken)
          .values({
            id: crypto.randomUUID(),
            providerId: input.providerId,
            tokenHash: hash,
            label: input.label ?? null,
            createdBy: ctx.user.id,
          })
          .returning({ id: providerToken.id, createdAt: providerToken.createdAt });

        await audit(ctx.db, ctx.user.id, "provider.token.create", "provider", input.providerId, {
          tokenId: created!.id,
          label: input.label,
        });

        return { id: created!.id, createdAt: created!.createdAt, token: plaintext };
      }),

    revoke: adminProcedure
      .input(z.object({ tokenId: z.string() }))
      .mutation(async ({ ctx, input }) => {
        const [revoked] = await ctx.db
          .update(providerToken)
          .set({ revokedAt: new Date() })
          .where(eq(providerToken.id, input.tokenId))
          .returning({ providerId: providerToken.providerId });
        if (!revoked) throw new TRPCError({ code: "NOT_FOUND" });

        await audit(ctx.db, ctx.user.id, "provider.token.revoke", "provider", revoked.providerId, {
          tokenId: input.tokenId,
        });
        return { ok: true };
      }),
  }),

  /** Ask a provider to tear down and re-establish a device's backend session. */
  restartDevice: adminProcedure
    .input(z.object({ deviceId: z.string() }))
    .mutation(async ({ ctx, input }) => {
      const [row] = await ctx.db
        .select({ providerId: device.providerId })
        .from(device)
        .where(eq(device.id, input.deviceId))
        .limit(1);
      if (!row) throw new TRPCError({ code: "NOT_FOUND" });

      await providers
        .require(row.providerId)
        .command({ kind: "device.restart", deviceId: input.deviceId });
      await audit(ctx.db, ctx.user.id, "device.restart", "device", input.deviceId);
      return { ok: true };
    }),
});
