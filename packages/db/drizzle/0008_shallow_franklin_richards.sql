CREATE TABLE "user_adb_key" (
	"id" text PRIMARY KEY NOT NULL,
	"user_id" text NOT NULL,
	"fingerprint" text NOT NULL,
	"public_key" text NOT NULL,
	"comment" text,
	"title" text NOT NULL,
	"created_at" timestamp DEFAULT now() NOT NULL,
	"last_used_at" timestamp
);
--> statement-breakpoint
ALTER TABLE "user_adb_key" ADD CONSTRAINT "user_adb_key_user_id_user_id_fk" FOREIGN KEY ("user_id") REFERENCES "public"."user"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
CREATE UNIQUE INDEX "user_adb_key_fingerprint_idx" ON "user_adb_key" USING btree ("fingerprint");--> statement-breakpoint
CREATE INDEX "user_adb_key_user_idx" ON "user_adb_key" USING btree ("user_id");