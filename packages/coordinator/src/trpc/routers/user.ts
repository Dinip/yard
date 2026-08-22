import { TRPCError } from "@trpc/server";
import { userAdbKey } from "@yard/db";
import { AdbKeyParseError, parseAdbPublicKey } from "@yard/protocol/adbkey";
import { and, desc, eq } from "drizzle-orm";
import { z } from "zod";
import { APP_NAME } from "../../app-name.ts";
import { authCapabilities } from "../../auth.ts";
import { pushAdbKeysForUser } from "../../lib/adb-keys.ts";
import { audit } from "../../lib/audit.ts";
import { isUniqueViolation } from "../../lib/pg-errors.ts";
import { protectedProcedure, publicProcedure, router } from "../init.ts";

/**
 * A user's own ADB keys.
 *
 * Registering one here is what makes `adb connect` silent: the provider is
 * pushed the key with the session and verifies the challenge locally, so
 * nothing is asked of anybody. Without one, the first connect parks and prompts
 * the holder.
 */
const adbKeysRouter = router({
  list: protectedProcedure.query(({ ctx }) =>
    ctx.db
      .select({
        id: userAdbKey.id,
        title: userAdbKey.title,
        fingerprint: userAdbKey.fingerprint,
        comment: userAdbKey.comment,
        createdAt: userAdbKey.createdAt,
        lastUsedAt: userAdbKey.lastUsedAt,
      })
      .from(userAdbKey)
      .where(eq(userAdbKey.userId, ctx.user.id))
      .orderBy(desc(userAdbKey.createdAt)),
  ),

  /** Takes the pasted contents of `~/.android/adbkey.pub`. */
  add: protectedProcedure
    .input(z.object({ publicKey: z.string().min(1), title: z.string().min(1).max(100) }))
    .mutation(async ({ ctx, input }) => {
      let parsed: ReturnType<typeof parseAdbPublicKey>;
      try {
        parsed = parseAdbPublicKey(input.publicKey);
      } catch (err) {
        if (err instanceof AdbKeyParseError) {
          throw new TRPCError({ code: "BAD_REQUEST", message: err.message });
        }
        throw err;
      }

      let created: typeof userAdbKey.$inferSelect | undefined;
      try {
        [created] = await ctx.db
          .insert(userAdbKey)
          .values({
            id: crypto.randomUUID(),
            userId: ctx.user.id,
            fingerprint: parsed.fingerprint,
            publicKey: parsed.publicKey,
            comment: parsed.comment ?? null,
            title: input.title,
          })
          .returning();
      } catch (err) {
        // The index is global, so this is either "you already added it" or
        // "somebody else did". We cannot say which without leaking whose it is,
        // and one key belonging to one person is the point.
        if (isUniqueViolation(err)) {
          throw new TRPCError({
            code: "CONFLICT",
            message: "That key is already registered",
          });
        }
        throw err;
      }

      // Any device this user is already in a session on should accept the key
      // now, rather than after their next reserve.
      await pushAdbKeysForUser(ctx.db, ctx.user.id);
      await audit(ctx.db, ctx.user.id, "user.adb_key.add", "user", ctx.user.id, {
        fingerprint: parsed.fingerprint,
      });
      return created!;
    }),

  remove: protectedProcedure
    .input(z.object({ id: z.string() }))
    .mutation(async ({ ctx, input }) => {
      const [removed] = await ctx.db
        .delete(userAdbKey)
        .where(and(eq(userAdbKey.id, input.id), eq(userAdbKey.userId, ctx.user.id)))
        .returning({ fingerprint: userAdbKey.fingerprint });
      if (!removed) throw new TRPCError({ code: "NOT_FOUND" });

      // Live sessions are the whole reason this is a push: a key deleted here
      // must stop working now, not when the reservation happens to end.
      await pushAdbKeysForUser(ctx.db, ctx.user.id);
      await audit(ctx.db, ctx.user.id, "user.adb_key.remove", "user", ctx.user.id, {
        fingerprint: removed.fingerprint,
      });
      return { ok: true };
    }),
});

export const userRouter = router({
  me: publicProcedure.query(({ ctx }) => {
    if (!ctx.user) return null;
    return {
      id: ctx.user.id,
      name: ctx.user.name,
      email: ctx.user.email,
      image: ctx.user.image ?? null,
      role: ctx.user.role ?? "user",
      isAdmin: ctx.user.role === "admin",
    };
  }),

  /** Lets the sign-in page render only the methods that are actually configured. */
  capabilities: publicProcedure.query(() => ({
    ...authCapabilities,
    appName: APP_NAME,
  })),

  adbKeys: adbKeysRouter,
});
