import { createFileRoute } from "@tanstack/react-router";
import { useEffect } from "react";
import { z } from "zod";
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import {
  Select,
  SelectContent,
  SelectItem,
  SelectTrigger,
  SelectValue,
} from "@/components/ui/select";
import { useChangelog } from "@/hooks/use-changelog";
import { VERSION } from "@/lib/build-info";
import { parseInlineNote } from "@/lib/changelog";
import { releases } from "@/lib/releases";

export const Route = createFileRoute("/_app/changelog")({
  validateSearch: z.object({ version: z.string().optional() }),
  component: ChangelogPage,
});

function ChangelogPage() {
  const { version = VERSION } = Route.useSearch();
  const navigate = Route.useNavigate();
  const release = releases.find((entry) => entry.version === version);

  return (
    <div className="mx-auto grid w-full max-w-3xl gap-6">
      <div className="flex flex-wrap items-center justify-between gap-4">
        <div>
          <h1 className="font-semibold text-2xl">Changelog</h1>
          <p className="text-muted-foreground text-sm">What's changed in YARD - Device Farm.</p>
        </div>
        <Select value={version} onValueChange={(version) => navigate({ search: { version } })}>
          <SelectTrigger aria-label="Release version" className="w-44">
            <SelectValue placeholder={`v${version}`} />
          </SelectTrigger>
          <SelectContent>
            {releases.map((entry) => (
              <SelectItem key={entry.version} value={entry.version}>
                v{entry.version}
                {entry.version === VERSION ? " · Installed" : ""}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
      </div>

      {release ? (
        <article className="grid gap-6 rounded-lg border bg-card p-6">
          <header className="flex flex-wrap items-center gap-3">
            <h2 className="font-semibold text-xl">v{release.version}</h2>
            {version === VERSION && <Badge variant="secondary">Installed version</Badge>}
            <time dateTime={release.date} className="text-muted-foreground text-sm">
              {release.date}
            </time>
          </header>
          {release.sections.map((section) => (
            <section key={section.title} className="grid gap-3">
              <h3 className="font-medium">{section.title}</h3>
              <ul className="list-disc space-y-2 pl-5 text-sm leading-relaxed">
                {section.items.map((item) => (
                  <li key={item}>
                    <Note text={item} />
                  </li>
                ))}
              </ul>
            </section>
          ))}
          <ViewedRelease key={version} version={version} />
        </article>
      ) : (
        <p className="rounded-lg border p-6 text-muted-foreground text-sm">
          Release notes for v{version} aren't available in this build. Choose another version above.
        </p>
      )}
    </div>
  );
}

function ViewedRelease({ version }: { version: string }) {
  const { viewed, markViewed } = useChangelog(version);
  const { mutate, status } = markViewed;
  useEffect(() => {
    if (viewed.data?.viewed === false && status === "idle") mutate({ version });
  }, [viewed.data?.viewed, status, mutate, version]);

  if (!viewed.isError && !markViewed.isError) return null;
  return (
    <div role="alert" className="flex flex-wrap items-center gap-3 text-destructive text-sm">
      <p>Couldn't save that you've viewed this release.</p>
      <Button variant="outline" size="sm" onClick={() => mutate({ version })}>
        Try again
      </Button>
    </div>
  );
}

function Note({ text }: { text: string }) {
  return parseInlineNote(text).map((part, index) => {
    const key = `${index}-${part.kind}`;
    if (part.kind === "strong") return <strong key={key}>{part.text}</strong>;
    if (part.kind === "code") {
      return (
        <code key={key} className="rounded bg-muted px-1 font-mono text-xs">
          {part.text}
        </code>
      );
    }
    if (part.kind === "link") {
      return (
        <a
          key={key}
          href={part.href}
          target="_blank"
          rel="noreferrer"
          className="text-primary underline underline-offset-4"
        >
          {part.text}
        </a>
      );
    }
    return part.text;
  });
}
