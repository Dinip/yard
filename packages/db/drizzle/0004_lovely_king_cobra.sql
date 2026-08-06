CREATE TABLE "reservation_observer" (
	"id" text PRIMARY KEY NOT NULL,
	"reservation_id" text NOT NULL,
	"user_id" text NOT NULL,
	"joined_at" timestamp DEFAULT now() NOT NULL,
	"left_at" timestamp
);
--> statement-breakpoint
ALTER TABLE "reservation_observer" ADD CONSTRAINT "reservation_observer_reservation_id_reservation_id_fk" FOREIGN KEY ("reservation_id") REFERENCES "public"."reservation"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
ALTER TABLE "reservation_observer" ADD CONSTRAINT "reservation_observer_user_id_user_id_fk" FOREIGN KEY ("user_id") REFERENCES "public"."user"("id") ON DELETE cascade ON UPDATE no action;--> statement-breakpoint
CREATE INDEX "reservation_observer_reservation_idx" ON "reservation_observer" USING btree ("reservation_id");--> statement-breakpoint
CREATE UNIQUE INDEX "reservation_observer_one_open_per_user" ON "reservation_observer" USING btree ("reservation_id","user_id") WHERE "reservation_observer"."left_at" is null;