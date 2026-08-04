import { initTRPC, TRPCError } from "@trpc/server";
import type { FetchCreateContextFnOptions } from "@trpc/server/adapters/fetch";
import { auth } from "../auth.ts";
import { db } from "../db.ts";

export async function createContext({ req }: FetchCreateContextFnOptions) {
  const session = await auth.api.getSession({ headers: req.headers });
  return { db, req, session: session?.session ?? null, user: session?.user ?? null };
}

export type Context = Awaited<ReturnType<typeof createContext>>;

const t = initTRPC.context<Context>().create();

export const router = t.router;
export const publicProcedure = t.procedure;

export const protectedProcedure = t.procedure.use(({ ctx, next }) => {
  if (!ctx.user) throw new TRPCError({ code: "UNAUTHORIZED" });
  if (ctx.user.banned) throw new TRPCError({ code: "FORBIDDEN", message: "Account is banned" });
  return next({ ctx: { ...ctx, user: ctx.user, session: ctx.session! } });
});

export const adminProcedure = protectedProcedure.use(({ ctx, next }) => {
  if (ctx.user.role !== "admin") throw new TRPCError({ code: "FORBIDDEN" });
  return next({ ctx });
});
