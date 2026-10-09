export interface ReleaseNotes {
  version: string;
  date: string;
  sections: { title: string; items: string[] }[];
}

/** Release-please's headings and bullets, keeping its inline Markdown for the UI. */
export function parseChangelog(markdown: string): ReleaseNotes[] {
  const releases: ReleaseNotes[] = [];
  let release: ReleaseNotes | undefined;
  let section: ReleaseNotes["sections"][number] | undefined;

  for (const line of markdown.split(/\r?\n/)) {
    const heading = line.match(/^## (?:\[([^\]]+)\]\([^)]*\)|(\S+)) \((\d{4}-\d{2}-\d{2})\)$/);
    if (heading) {
      release = { version: heading[1] ?? heading[2]!, date: heading[3]!, sections: [] };
      releases.push(release);
      section = undefined;
    } else if (release && line.startsWith("### ")) {
      section = { title: line.slice(4), items: [] };
      release.sections.push(section);
    } else if (section && /^[*-] /.test(line)) {
      section.items.push(line.slice(2));
    } else if (section?.items.length && line.trim()) {
      const last = section.items.length - 1;
      section.items[last] += ` ${line.trim()}`;
    }
  }
  return releases;
}

export type InlineNote = { text: string; kind: "text" | "code" | "strong" | "link"; href?: string };

export function parseInlineNote(markdown: string): InlineNote[] {
  return markdown.split(/(\[[^\]]+\]\([^)]*\)|\*\*[^*]+\*\*|`[^`]+`)/g).map((text) => {
    const link = text.match(/^\[([^\]]+)\]\((https?:\/\/[^\s]+)\)$/);
    if (link) return { kind: "link", text: link[1]!, href: link[2]! };
    if (text.startsWith("**") && text.endsWith("**")) {
      return { kind: "strong", text: text.slice(2, -2) };
    }
    if (text.startsWith("`") && text.endsWith("`")) {
      return { kind: "code", text: text.slice(1, -1) };
    }
    return { kind: "text", text };
  });
}
