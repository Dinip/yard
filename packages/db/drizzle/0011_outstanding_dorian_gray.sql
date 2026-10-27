ALTER TABLE "device" ADD COLUMN "maintenance_reason" text;--> statement-breakpoint
ALTER TABLE "provider" ADD COLUMN "draining" boolean DEFAULT false NOT NULL;