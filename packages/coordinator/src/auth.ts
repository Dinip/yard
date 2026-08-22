import { schema } from "@yard/db";
import { betterAuth } from "better-auth";
import { drizzleAdapter } from "better-auth/adapters/drizzle";
import { admin } from "better-auth/plugins";
import { db } from "./db.ts";
import { env } from "./env.ts";

const microsoftConfigured = !!(env.MICROSOFT_CLIENT_ID && env.MICROSOFT_CLIENT_SECRET);

export const auth = betterAuth({
  appName: env.APP_NAME,
  baseURL: env.PUBLIC_URL,
  basePath: "/api/auth",
  secret: env.AUTH_SECRET,
  trustedOrigins: [env.PUBLIC_URL, ...env.WEB_ORIGIN],

  database: drizzleAdapter(db, {
    provider: "pg",
    schema: {
      user: schema.user,
      session: schema.session,
      account: schema.account,
      verification: schema.verification,
    },
  }),

  emailAndPassword: {
    enabled: env.ENABLE_EMAIL_PASSWORD,
    // No mail transport is wired up yet; verification would strand users.
    requireEmailVerification: false,
  },

  socialProviders: microsoftConfigured
    ? {
        microsoft: {
          clientId: env.MICROSOFT_CLIENT_ID!,
          clientSecret: env.MICROSOFT_CLIENT_SECRET!,
          tenantId: env.MICROSOFT_TENANT_ID,
        },
      }
    : {},

  session: {
    expiresIn: 60 * 60 * 24 * 7,
    updateAge: 60 * 60 * 24,
    cookieCache: { enabled: true, maxAge: 60 },
  },

  databaseHooks: {
    user: {
      create: {
        // A fresh deployment has nobody who can promote anyone, so the account
        // that creates the farm owns it. `grant-admin` stays for every case
        // after this one.
        before: async (data) => {
          const [existing] = await db.select({ id: schema.user.id }).from(schema.user).limit(1);
          return existing ? { data } : { data: { ...data, role: "admin" } };
        },
      },
    },
  },

  plugins: [admin()],
});

export type AuthSession = typeof auth.$Infer.Session;

export const authCapabilities = {
  microsoft: microsoftConfigured,
  emailPassword: env.ENABLE_EMAIL_PASSWORD,
};
