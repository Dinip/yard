import { createEnv } from "@t3-oss/env-core";
import { z } from "zod";

export const env = createEnv({
  server: {
    NODE_ENV: z.enum(["development", "test", "production"]).default("development"),
    PORT: z.coerce.number().int().default(3000),

    DATABASE_URL: z.string().url(),

    /** Public origin of the coordinator itself, e.g. https://farm.example.com */
    PUBLIC_URL: z.string().url(),
    /** Origin(s) allowed to call the API with credentials — the web SPA. */
    WEB_ORIGIN: z
      .string()
      .default("http://localhost:5173")
      .transform((s) => s.split(",").map((v) => v.trim())),

    /** better-auth signing secret. Generate with: openssl rand -base64 32 */
    AUTH_SECRET: z.string().min(32),

    /** Entra ID / Microsoft app registration. Omit to disable social sign-in. */
    MICROSOFT_CLIENT_ID: z.string().optional(),
    MICROSOFT_CLIENT_SECRET: z.string().optional(),
    MICROSOFT_TENANT_ID: z.string().default("common"),

    /**
     * Email+password sign-in. Intended for bootstrapping the first admin and for
     * local development; leave off once Microsoft sign-in works.
     */
    ENABLE_EMAIL_PASSWORD: z
      .string()
      .default("true")
      .transform((s) => s === "true"),

    /** Ed25519 PKCS#8 PEM used to sign session tokens. Generated at boot if unset. */
    SESSION_TOKEN_PRIVATE_KEY: z.string().optional(),
    /** Session-token lifetime in seconds. Short: the client refreshes. */
    SESSION_TOKEN_TTL: z.coerce.number().int().default(60),

    /** Default reservation lifetime in seconds, renewed while the tab is open. */
    RESERVATION_TTL: z.coerce.number().int().default(900),

    /** Display name of the product. Centralised here so a rename is one variable. */
    APP_NAME: z.string().default("Device Farm"),
  },
  runtimeEnv: process.env,
  emptyStringAsUndefined: true,
});

export type Env = typeof env;
