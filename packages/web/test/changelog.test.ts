import { describe, expect, test } from "bun:test";
import pkg from "../package.json";
import { parseChangelog, parseInlineNote } from "../src/lib/changelog.ts";

describe("release-please changelog", () => {
  test("keeps versions, sections, inline Markdown and wrapped bullets apart", () => {
    const releases = parseChangelog(`# Changelog

## [1.2.0](https://example.com/compare) (2026-10-09)
### Features
* **web:** add \`changelog\` ([#1](https://example.com/1))
  with wrapped text
### Bug Fixes
* fix input
## 1.1.0 (2026-10-01)
### Features
* first release
`);
    expect(releases).toEqual([
      {
        version: "1.2.0",
        date: "2026-10-09",
        sections: [
          {
            title: "Features",
            items: ["**web:** add `changelog` ([#1](https://example.com/1)) with wrapped text"],
          },
          { title: "Bug Fixes", items: ["fix input"] },
        ],
      },
      {
        version: "1.1.0",
        date: "2026-10-01",
        sections: [{ title: "Features", items: ["first release"] }],
      },
    ]);
  });

  test("the installed version has notes, including the unlinked first release", async () => {
    const source = await Bun.file(new URL("../../../CHANGELOG.md", import.meta.url)).text();
    const releases = parseChangelog(source);
    expect(
      releases.find((release) => release.version === pkg.version)?.sections.length,
    ).toBeGreaterThan(0);
    expect(releases.at(-1)?.version).toBe("0.1.0");
  });
});

describe("inline notes", () => {
  test("recognises formatting and HTTP links", () => {
    expect(
      parseInlineNote("**web:** use `key` [issue](https://example.com/1)").filter(
        (part) => part.kind !== "text",
      ),
    ).toEqual([
      { kind: "strong", text: "web:" },
      { kind: "code", text: "key" },
      { kind: "link", text: "issue", href: "https://example.com/1" },
    ]);
  });

  test("HTML and executable links stay plain text", () => {
    const source = "<img src=x onerror=alert(1)> [bad](javascript:alert)";
    expect(parseInlineNote(source).every((part) => part.kind === "text")).toBe(true);
    expect(
      parseInlineNote(source)
        .map((part) => part.text)
        .join(""),
    ).toBe(source);
  });
});
