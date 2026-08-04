CREATE TYPE "public"."device_status" AS ENUM('absent', 'present', 'preparing', 'ready', 'busy', 'unhealthy');--> statement-breakpoint
CREATE TYPE "public"."platform" AS ENUM('ios', 'android');--> statement-breakpoint
CREATE TYPE "public"."provider_status" AS ENUM('online', 'offline');--> statement-breakpoint
CREATE TYPE "public"."reservation_state" AS ENUM('active', 'released', 'expired');--> statement-breakpoint
CREATE TABLE "account" (
	"id" text PRIMARY KEY NOT NULL,
	"account_id" text NOT NULL,
	"provider_id" text NOT NULL,
	"user_id" text NOT NULL,
	"access_token" text,
	"refresh_token" text,
	"id_token" text,
	"access_token_expires_at" timestamp,
	"refresh_token_expires_at" timestamp,
	"scope" text,
	"password" text,
	"created_at" timestamp NOT NULL,
	"updated_at" timestamp NOT NULL
);
--> statement-breakpoint
CREATE TABLE "session" (
	"id" text PRIMARY KEY NOT NULL,
	"expires_at" timestamp NOT NULL,
	"token" text NOT NULL,
	"created_at" timestamp NOT NULL,
	"updated_at" timestamp NOT NULL,
	"ip_address" text,
	"user_agent" text,
	"user_id" text NOT NULL,
	"impersonated_by" text,
	CONSTRAINT "session_token_unique" UNIQUE("token")
);
--> statement-breakpoint
CREATE TABLE "user" (
	"id" text PRIMARY KEY NOT NULL,
	"name" text NOT NULL,
	"email" text NOT NULL,
	"email_verified" boolean NOT NULL,
	"image" text,
	"created_at" timestamp NOT NULL,
	"updated_at" timestamp NOT NULL,
	"role" text DEFAULT 'user',
	"banned" boolean DEFAULT false,
	"ban_reason" text,
	"ban_expires" timestamp,
	CONSTRAINT "user_email_unique" UNIQUE("email")
);
--> statement-breakpoint
CREATE TABLE "verification" (
	"id" text PRIMARY KEY NOT NULL,
	"identifier" text NOT NULL,
	"value" text NOT NULL,
	"expires_at" timestamp NOT NULL,
	"created_at" timestamp,
	"updated_at" timestamp
);
--> statement-breakpoint
CREATE TABLE "audit_log" (
	"id" text PRIMARY KEY NOT NULL,
	"actor_user_id" text,
	"action" text NOT NULL,
	"target_type" text,
	"target_id" text,
	"metadata" jsonb,
	"at" timestamp DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE TABLE "device" (
	"id" text PRIMARY KEY NOT NULL,
	"provider_id" text NOT NULL,
	"platform" "platform" NOT NULL,
	"name" text,
	"model" text,
	"manufacturer" text,
	"os_version" text,
	"abi" text,
	"sdk" integer,
	"display_width" integer,
	"display_height" integer,
	"display_scale" real,
	"display_rotation" integer,
	"battery_level" real,
	"battery_state" text,
	"status" "device_status" DEFAULT 'absent' NOT NULL,
	"stream_codec" text,
	"adb_port" integer,
	"present_at" timestamp,
	"ready_at" timestamp,
	"notes" text,
	"created_at" timestamp DEFAULT now() NOT NULL,
	"updated_at" timestamp DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE TABLE "provider" (
	"id" text PRIMARY KEY NOT NULL,
	"name" text NOT NULL,
	"public_base_url" text NOT NULL,
	"hostname" text,
	"version" text,
	"status" "provider_status" DEFAULT 'offline' NOT NULL,
	"last_seen_at" timestamp,
	"created_at" timestamp DEFAULT now() NOT NULL,
	"updated_at" timestamp DEFAULT now() NOT NULL
);
--> statement-breakpoint
CREATE TABLE "provider_token" (
	"id" text PRIMARY KEY NOT NULL,
	"provider_id" text NOT NULL,
	"token_hash" text NOT NULL,
	"label" text,
	"created_by" text,
	"created_at" timestamp DEFAULT now() NOT NULL,
	"last_used_at" timestamp,
	"revoked_at" timestamp,
	CONSTRAINT "provider_token_token_hash_unique" UNIQUE("token_hash")
);
--> statement-breakpoint
CREATE TABLE "reservation" (
	"id" text PRIMARY KEY NOT NULL,
	"device_id" text NOT NULL,
	"user_id" text NOT NULL,
	"state" "reservation_state" DEFAULT 'active' NOT NULL,
	"started_at" timestamp DEFAULT now() NOT NULL,
	"expires_at" timestamp NOT NULL,
	"released_at" timestamp,
	"released_by" text,
	"reason" text
);
--> statement-breakpoint
ALTER TABLE "account" ADD CONSTRAINT "account_user_id_user_id_fk" FOREIGN KEY ("user_id") REFERENCES "public"."user"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "session" ADD CONSTRAINT "session_user_id_user_id_fk" FOREIGN KEY ("user_id") REFERENCES "public"."user"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "audit_log" ADD CONSTRAINT "audit_log_actor_user_id_user_id_fk" FOREIGN KEY ("actor_user_id") REFERENCES "public"."user"("id") ON DELETE set null ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "device" ADD CONSTRAINT "device_provider_id_provider_id_fk" FOREIGN KEY ("provider_id") REFERENCES "public"."provider"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "provider_token" ADD CONSTRAINT "provider_token_provider_id_provider_id_fk" FOREIGN KEY ("provider_id") REFERENCES "public"."provider"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "provider_token" ADD CONSTRAINT "provider_token_created_by_user_id_fk" FOREIGN KEY ("created_by") REFERENCES "public"."user"("id") ON DELETE set null ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "reservation" ADD CONSTRAINT "reservation_device_id_device_id_fk" FOREIGN KEY ("device_id") REFERENCES "public"."device"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "reservation" ADD CONSTRAINT "reservation_user_id_user_id_fk" FOREIGN KEY ("user_id") REFERENCES "public"."user"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "reservation" ADD CONSTRAINT "reservation_released_by_user_id_fk" FOREIGN KEY ("released_by") REFERENCES "public"."user"("id") ON DELETE set null ON UPDATE no action;--> statement-breakpoint
CREATE INDEX "audit_log_at_idx" ON "audit_log" USING btree ("at");--> statement-breakpoint
CREATE INDEX "audit_log_actor_idx" ON "audit_log" USING btree ("actor_user_id");--> statement-breakpoint
CREATE INDEX "device_provider_idx" ON "device" USING btree ("provider_id");--> statement-breakpoint
CREATE INDEX "device_status_idx" ON "device" USING btree ("status");--> statement-breakpoint
CREATE INDEX "provider_token_provider_idx" ON "provider_token" USING btree ("provider_id");--> statement-breakpoint
CREATE UNIQUE INDEX "reservation_one_active_per_device" ON "reservation" USING btree ("device_id") WHERE "reservation"."state" = 'active';--> statement-breakpoint
CREATE INDEX "reservation_user_idx" ON "reservation" USING btree ("user_id");--> statement-breakpoint
CREATE INDEX "reservation_expires_idx" ON "reservation" USING btree ("expires_at") WHERE "reservation"."state" = 'active';