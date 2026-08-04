import { authCapabilities } from "../../auth.ts";
import { env } from "../../env.ts";
import { publicProcedure, router } from "../init.ts";

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
    appName: env.APP_NAME,
  })),
});
