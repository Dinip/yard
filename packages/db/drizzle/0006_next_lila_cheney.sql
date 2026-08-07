CREATE TYPE "public"."join_request_state" AS ENUM('pending', 'approved', 'denied', 'cancelled', 'expired');--> statement-breakpoint
CREATE TABLE "join_request" (
	"id" text PRIMARY KEY NOT NULL,
	"reservation_id" text NOT NULL,
	"user_id" text NOT NULL,
	"state" "join_request_state" DEFAULT 'pending' NOT NULL,
	"note" text,
	"created_at" timestamp DEFAULT now() NOT NULL,
	"expires_at" timestamp NOT NULL,
	"decided_at" timestamp,
	"decided_by" text
);
--> statement-breakpoint
ALTER TABLE "join_request" ADD CONSTRAINT "join_request_reservation_id_reservation_id_fk" FOREIGN KEY ("reservation_id") REFERENCES "public"."reservation"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "join_request" ADD CONSTRAINT "join_request_user_id_user_id_fk" FOREIGN KEY ("user_id") REFERENCES "public"."user"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "join_request" ADD CONSTRAINT "join_request_decided_by_user_id_fk" FOREIGN KEY ("decided_by") REFERENCES "public"."user"("id") ON DELETE set null ON UPDATE no action;--> statement-breakpoint
CREATE INDEX "join_request_reservation_idx" ON "join_request" USING btree ("reservation_id");--> statement-breakpoint
CREATE UNIQUE INDEX "join_request_one_pending_per_user" ON "join_request" USING btree ("reservation_id","user_id") WHERE "join_request"."state" = 'pending';--> statement-breakpoint
CREATE INDEX "join_request_pending_expiry_idx" ON "join_request" USING btree ("expires_at") WHERE "join_request"."state" = 'pending';