import { setting, user } from "@farm/db";
import { TRPCError } from "@trpc/server";
import { desc, eq } from "drizzle-orm";
import { z } from "zod";
import { audit } from "../../lib/audit.ts";
import {
  defaults,
  getSettings,
  parseSetting,
  SETTING_KEYS,
  type SettingKey,
  setSetting,
} from "../../lib/settings.ts";
import { adminProcedure, protectedProcedure, router } from "../init.ts";

const settingKey = z.enum(SETTING_KEYS as [SettingKey, ...SettingKey[]]);

export const settingsRouter = router({
  /** Everything, with who changed what — the admin page. */
  get: adminProcedure.query(async ({ ctx }) => {
    const [values, rows] = await Promise.all([
      getSettings(ctx.db),
      ctx.db
        .select({
          key: setting.key,
          updatedAt: setting.updatedAt,
          updatedByName: user.name,
        })
        .from(setting)
        .leftJoin(user, eq(setting.updatedBy, user.id))
        .orderBy(desc(setting.updatedAt)),
    ]);

    return { values, defaults: defaults(), changed: rows };
  }),

  /**
   * The subset every signed-in user needs.
   *
   * The idle countdown is rendered client-side, so the browser has to know the
   * policy. Deliberately a separate procedure rather than relaxing `get`: what
   * a user may read is a smaller thing than what an admin may edit, and that
   * should be visible in the router rather than inside a branch.
   */
  public: protectedProcedure.query(async ({ ctx }) => {
    const values = await getSettings(ctx.db);
    return {
      idleTimeoutSeconds: values["reservation.idleTimeoutSeconds"],
      maxDurationSeconds: values["reservation.maxDurationSeconds"],
      ttlSeconds: values["reservation.ttlSeconds"],
    };
  }),

  set: adminProcedure
    .input(z.object({ key: settingKey, value: z.unknown() }))
    .mutation(async ({ ctx, input }) => {
      const parsed = parseSetting(input.key, input.value);
      if (!parsed.success) {
        throw new TRPCError({ code: "BAD_REQUEST", message: parsed.error.issues[0]?.message });
      }

      await setSetting(ctx.db, input.key, parsed.data, ctx.user.id);
      await audit(ctx.db, ctx.user.id, "settings.update", "setting", input.key, {
        value: parsed.data,
      });

      return getSettings(ctx.db);
    }),
});
