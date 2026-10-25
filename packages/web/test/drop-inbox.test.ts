/**
 * The inbox reader's whole job is deciding what a browser may show. A batch
 * shown too early is a half-written file downloaded as if it were whole, so
 * most of what is checked here is what it refuses.
 */

import { describe, expect, test } from "bun:test";
import type { FileListing } from "@yard/protocol";
import {
  armIosDrop,
  INBOX_PATH,
  type InboxSource,
  IOS_INBOX_PATH,
  iosDropBundle,
  publishIosDrop,
  readInbox,
} from "../src/lib/drop-inbox.ts";

/** A device whose inbox is described as `{ "<batch>": [names…] }`. */
function device(
  batches: Record<string, string[]>,
  manifests: Record<string, unknown> = {},
  root = INBOX_PATH,
): InboxSource {
  const listing = (path: string, names: string[], kind: "file" | "directory"): FileListing => ({
    path,
    parent: null,
    entries: names.map((name) => ({ name, path: `${path}/${name}`, kind, size: 4 })),
  });

  return {
    async list(path) {
      if (path === root) return listing(path, Object.keys(batches), "directory");
      const id = path.slice(root.length + 1);
      const names = batches[id];
      if (!names) throw new Error("No such directory");
      return listing(path, names, "file");
    },
    async read(path) {
      const id = path.slice(root.length + 1).split("/")[0];
      const manifest = manifests[id];
      if (manifest === undefined) throw new Error("No such file");
      return new Blob([typeof manifest === "string" ? manifest : JSON.stringify(manifest)]);
    },
  };
}

function manifest(files: { name: string; size?: number }[], overrides: object = {}) {
  return {
    schemaVersion: 1,
    batchId: "20260905-101500-abc",
    createdAt: 1_767_610_500_000,
    producer: { appVersion: "0.1.0", buildNumber: 1, commit: "abc1234" },
    files: files.map((file) => ({
      name: file.name,
      mimeType: "application/zip",
      size: file.size ?? 4,
    })),
    ...overrides,
  };
}

const ready = "_YARD_READY";

test("iOS finds a companion signed under the user's bundle ID", () => {
  expect(iosDropBundle([{ id: "com.example.signed.drop", name: "YARD - Device Farm" }])).toBe(
    "com.example.signed.drop",
  );
  expect(() => iosDropBundle([])).toThrow("Install YARD Drop");
  expect(() =>
    iosDropBundle([
      { id: "com.example.one", name: "yard_drop" },
      { id: "com.example.two", name: "YARD - Device Farm" },
    ]),
  ).toThrow("Several YARD Drop");
});

test("iOS reads only the armed capture's House Arrest subtree", async () => {
  const root = `${IOS_INBOX_PATH}/capture-id`;
  const source = device(
    { batch: ["report.zip", "batch.json", ready] },
    { batch: manifest([{ name: "report.zip" }]) },
    root,
  );
  const [batch] = await readInbox(source, new Map(), root);
  expect(batch.files[0].path).toBe(`${root}/batch/report.zip`);
});

test("iOS prepares a fresh capture and publishes with the same secret", async () => {
  const commands: string[][] = [];
  const launch = async (args: string[]) => {
    commands.push(args);
  };
  const capture = await armIosDrop(launch);
  expect(capture.id).toMatch(/^[0-9a-f-]{36}$/);
  expect(capture.expiresAt).toBeGreaterThan(Date.now());
  await publishIosDrop(launch, capture);
  expect(commands).toEqual([
    ["--yard-drop", "arm", capture.id, String(capture.expiresAt)],
    ["--yard-drop", "publish", capture.id],
  ]);
});

test("failed preparation and expired captures cannot proceed to publishing", async () => {
  await expect(
    armIosDrop(async () => {
      throw new Error("Device unavailable");
    }),
  ).rejects.toThrow("Device unavailable");
  let launches = 0;
  await expect(
    publishIosDrop(
      async () => {
        launches++;
      },
      { id: crypto.randomUUID(), expiresAt: Date.now() - 1 },
    ),
  ).rejects.toThrow("expired");
  expect(launches).toBe(0);
});

test("an inbox that does not exist yet is empty", async () => {
  const source: InboxSource = {
    list: () => Promise.reject(new Error("No such file or directory")),
    read: () => Promise.reject(new Error("No such file or directory")),
  };
  expect(await readInbox(source)).toEqual([]);
});

test("an inbox with no batches is empty", async () => {
  expect(await readInbox(device({}))).toEqual([]);
});

test("a batch without the marker is not shown", async () => {
  const source = device(
    { "20260905-101500-a": ["report.zip", "batch.json"] },
    { "20260905-101500-a": manifest([{ name: "report.zip" }]) },
  );
  expect(await readInbox(source)).toEqual([]);
});

test("a complete batch lists its files with device paths", async () => {
  const source = device(
    { "20260905-101500-a": ["report.zip", "batch.json", ready] },
    { "20260905-101500-a": manifest([{ name: "report.zip", size: 91 }]) },
  );

  const [batch] = await readInbox(source);
  expect(batch.id).toBe("20260905-101500-a");
  expect(batch.createdAt).toBe(1_767_610_500_000);
  expect(batch.error).toBeUndefined();
  expect(batch.files).toEqual([
    {
      name: "report.zip",
      path: `${INBOX_PATH}/20260905-101500-a/report.zip`,
      mimeType: "application/zip",
      size: 91,
    },
  ]);
});

test("a batch keeps every file the manifest names", async () => {
  const source = device(
    { "20260905-101500-a": ["a.zip", "b.pdf", "c.jpg", "batch.json", ready] },
    {
      "20260905-101500-a": manifest([{ name: "a.zip" }, { name: "b.pdf" }, { name: "c.jpg" }]),
    },
  );

  const [batch] = await readInbox(source);
  expect(batch.files.map((file) => file.name)).toEqual(["a.zip", "b.pdf", "c.jpg"]);
});

test("a file named by the manifest but missing from the device is not offered", async () => {
  const source = device(
    { "20260905-101500-a": ["a.zip", "batch.json", ready] },
    { "20260905-101500-a": manifest([{ name: "a.zip" }, { name: "deleted.pdf" }]) },
  );

  const [batch] = await readInbox(source);
  expect(batch.files.map((file) => file.name)).toEqual(["a.zip"]);
});

test("several batches come back newest first", async () => {
  const source = device(
    {
      "20260905-101500-a": ["a.zip", "batch.json", ready],
      "20260905-120000-b": ["b.zip", "batch.json", ready],
      "20260904-090000-c": ["c.zip", "batch.json", ready],
    },
    {
      "20260905-101500-a": manifest([{ name: "a.zip" }]),
      "20260905-120000-b": manifest([{ name: "b.zip" }]),
      "20260904-090000-c": manifest([{ name: "c.zip" }]),
    },
  );

  expect((await readInbox(source)).map((batch) => batch.id)).toEqual([
    "20260905-120000-b",
    "20260905-101500-a",
    "20260904-090000-c",
  ]);
});

describe("a batch this console cannot read", () => {
  test("names the build that wrote a newer schema", async () => {
    const source = device(
      { "20260905-101500-a": ["a.zip", "batch.json", ready] },
      {
        "20260905-101500-a": manifest([{ name: "a.zip" }], {
          schemaVersion: 2,
          producer: { appVersion: "9.9.9", buildNumber: 99, commit: "deadbee" },
        }),
      },
    );

    const [batch] = await readInbox(source);
    expect(batch.error).toContain("9.9.9");
    expect(batch.files).toEqual([]);
  });

  test("says so when the manifest is not JSON", async () => {
    const source = device(
      { "20260905-101500-a": ["a.zip", "batch.json", ready] },
      { "20260905-101500-a": "half a file" },
    );

    const [batch] = await readInbox(source);
    expect(batch.error).toBe("This batch's manifest could not be read.");
  });

  test("says so when the marker is there but the manifest is not", async () => {
    const source = device({ "20260905-101500-a": ["a.zip", ready] });

    const [batch] = await readInbox(source);
    expect(batch.error).toBe("This batch has no manifest.");
  });
});

test("a batch that disappears mid-read is skipped rather than failing the poll", async () => {
  const source = device(
    {
      "20260905-101500-a": ["a.zip", "batch.json", ready],
      "20260905-120000-gone": [],
    },
    { "20260905-101500-a": manifest([{ name: "a.zip" }]) },
  );
  // `device` throws for a directory it has no names for, which is what the
  // provider does once a reservation's cleanup has removed it.
  const listing = source.list.bind(source);
  source.list = (path) =>
    path.endsWith("gone") ? Promise.reject(new Error("gone")) : listing(path);

  expect((await readInbox(source)).map((batch) => batch.id)).toEqual(["20260905-101500-a"]);
});

describe("polling the same device", () => {
  /** Wraps a source, counting the manifest reads a poll costs. */
  function counted(source: InboxSource) {
    let reads = 0;
    return {
      source: {
        list: source.list.bind(source),
        read: (path: string) => {
          reads += 1;
          return source.read(path);
        },
      },
      reads: () => reads,
    };
  }

  test("re-reads no manifest it has already seen", async () => {
    const { source, reads } = counted(
      device(
        { "20260905-101500-a": ["a.zip", "batch.json", ready] },
        { "20260905-101500-a": manifest([{ name: "a.zip" }]) },
      ),
    );
    const seen = new Map();

    const first = await readInbox(source, seen);
    const second = await readInbox(source, seen);

    // Once, not twice: a manifest read is an audited file pull, and a dialog
    // left open all afternoon would otherwise be the loudest thing in the log.
    expect(reads()).toBe(1);
    expect(second).toEqual(first);
  });

  test("reads a batch that appears between polls", async () => {
    const batches: Record<string, string[]> = {
      "20260905-101500-a": ["a.zip", "batch.json", ready],
    };
    const manifests: Record<string, unknown> = {
      "20260905-101500-a": manifest([{ name: "a.zip" }]),
      "20260905-120000-b": manifest([{ name: "b.zip" }]),
    };
    const source = device(batches, manifests);
    const seen = new Map();

    expect((await readInbox(source, seen)).length).toBe(1);
    batches["20260905-120000-b"] = ["b.zip", "batch.json", ready];
    expect((await readInbox(source, seen)).map((batch) => batch.id)).toEqual([
      "20260905-120000-b",
      "20260905-101500-a",
    ]);
  });

  test("forgets a batch that cleanup removed", async () => {
    const batches: Record<string, string[]> = {
      "20260905-101500-a": ["a.zip", "batch.json", ready],
    };
    const source = device(batches, { "20260905-101500-a": manifest([{ name: "a.zip" }]) });
    const seen = new Map();

    await readInbox(source, seen);
    delete batches["20260905-101500-a"];

    expect(await readInbox(source, seen)).toEqual([]);
    expect(seen.size).toBe(0);
  });
});
