CREATE TABLE "changelog_view" (
	"user_id" text NOT NULL,
	"version" text NOT NULL,
	CONSTRAINT "changelog_view_user_id_version_pk" PRIMARY KEY("user_id","version")
);
--> statement-breakpoint
ALTER TABLE "changelog_view" ADD CONSTRAINT "changelog_view_user_id_user_id_fk" FOREIGN KEY ("user_id") REFERENCES "public"."user"("id") ON DELETE cascade ON UPDATE no action;