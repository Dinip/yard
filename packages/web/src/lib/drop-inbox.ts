/**
 * Reading what YARD Drop left on an Android device.
 *
 * The companion app cannot reach the provider, so the two sides meet on the
 * filesystem: Drop writes a batch directory under [INBOX_PATH] and the browser
 * polls for it through the same authenticated listing the file browser uses.
 * Nothing here knows about the phone; it is directory reading with one rule.
 *
 * That rule is [READY_MARKER]. Drop writes it last, so a directory holding it
 * is promised every file its manifest names. A directory without it is either
 * still arriving or was abandoned, and either way is not ours to show.
 */

import type { FileEntry, FileListing } from "@yard/protocol";

/** Where the companion publishes, matching `ShareSaver.kt`. */
export const INBOX_PATH = "/sdcard/Download/YARD Drop/Inbox";

export const MANIFEST_NAME = "batch.json";
export const READY_MARKER = "_YARD_READY";

/** The newest `batch.json` shape this console can read. */
export const SUPPORTED_SCHEMA_VERSION = 1;

export interface DropFile {
  name: string;
  path: string;
  mimeType: string;
  size: number | undefined;
}

export interface DropBatch {
  id: string;
  path: string;
  /** Epoch millis from the manifest; undefined when it could not be read. */
  createdAt: number | undefined;
  files: DropFile[];
  /** Set instead of files when the batch is there but unreadable. */
  error?: string;
}

export interface InboxSource {
  list(path: string): Promise<FileListing>;
  read(path: string): Promise<Blob>;
}

/**
 * Every complete batch waiting on the device, newest first.
 *
 * A failure to list the inbox itself is an empty inbox, not an error: until
 * somebody shares something the directory does not exist, and that is the
 * state the dialog spends most of its life in.
 *
 * `seen` is a cache the caller keeps across polls, and it is not an
 * optimisation. Reading a manifest is a file pull, which writes an audit row —
 * without it a dialog left open would fill the log with its own polling. A
 * batch is immutable once its marker is there, so re-reading one would answer
 * the same thing anyway.
 */
export async function readInbox(
  source: InboxSource,
  seen: Map<string, DropBatch> = new Map(),
): Promise<DropBatch[]> {
  let listing: FileListing;
  try {
    listing = await source.list(INBOX_PATH);
  } catch {
    return [];
  }

  const batches: DropBatch[] = [];
  for (const entry of listing.entries) {
    if (entry.kind !== "directory") continue;
    const cached = seen.get(entry.path);
    if (cached) {
      batches.push(cached);
      continue;
    }
    const batch = await readBatch(source, entry);
    if (batch) {
      seen.set(entry.path, batch);
      batches.push(batch);
    }
  }

  // A batch removed from the device — by cleanup, or by the next reservation —
  // stops being listed, so the cache must not keep it alive.
  const present = new Set(batches.map((batch) => batch.path));
  for (const path of seen.keys()) if (!present.has(path)) seen.delete(path);

  // The directory name starts with a UTC timestamp, so it orders batches even
  // when a manifest could not be parsed.
  return batches.sort((a, b) => (a.id < b.id ? 1 : -1));
}

async function readBatch(source: InboxSource, directory: FileEntry): Promise<DropBatch | null> {
  let contents: FileListing;
  try {
    contents = await source.list(directory.path);
  } catch {
    // A batch that vanished between the two listings is not worth an error;
    // the next poll will simply not find it.
    return null;
  }

  if (!contents.entries.some((entry) => entry.name === READY_MARKER)) return null;

  const manifest = contents.entries.find((entry) => entry.name === MANIFEST_NAME);
  const base: DropBatch = {
    id: directory.name,
    path: directory.path,
    createdAt: undefined,
    files: [],
  };
  if (!manifest) return { ...base, error: "This batch has no manifest." };

  let parsed: unknown;
  try {
    parsed = JSON.parse(await (await source.read(manifest.path)).text());
  } catch {
    return { ...base, error: "This batch's manifest could not be read." };
  }

  return describe(base, parsed, contents.entries);
}

function describe(base: DropBatch, parsed: unknown, entries: FileEntry[]): DropBatch {
  if (typeof parsed !== "object" || parsed === null) {
    return { ...base, error: "This batch's manifest could not be read." };
  }
  const manifest = parsed as Record<string, unknown>;

  const version = manifest.schemaVersion;
  if (typeof version !== "number") {
    return { ...base, error: "This batch's manifest could not be read." };
  }
  if (version > SUPPORTED_SCHEMA_VERSION) {
    // Naming the build is the point: the phone is running a newer YARD Drop
    // than this console, and somebody has to be told which one.
    const producer = manifest.producer as Record<string, unknown> | undefined;
    const app = typeof producer?.appVersion === "string" ? producer.appVersion : "unknown";
    return {
      ...base,
      error: `This batch was written by YARD Drop ${app}, which this console is too old to read.`,
    };
  }

  const createdAt = typeof manifest.createdAt === "number" ? manifest.createdAt : undefined;
  const listed = new Map(entries.map((entry) => [entry.name, entry]));
  const files: DropFile[] = [];

  for (const item of Array.isArray(manifest.files) ? manifest.files : []) {
    const file = item as Record<string, unknown>;
    if (typeof file.name !== "string") continue;
    // The manifest names what the device wrote, but the listing is what is
    // actually there — a file removed since is not offered for download.
    const entry = listed.get(file.name);
    if (!entry) continue;
    files.push({
      name: file.name,
      path: entry.path,
      mimeType: typeof file.mimeType === "string" ? file.mimeType : "application/octet-stream",
      size: typeof file.size === "number" ? file.size : entry.size,
    });
  }

  return { ...base, createdAt, files };
}
