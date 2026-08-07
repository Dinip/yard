import { z } from "zod";
import { Timestamp } from "./common.ts";
import { named } from "./registry.ts";

/**
 * Artifact plane: browser ↔ provider, direct HTTPS at
 * `https://<publicBaseUrl>/s/<deviceId>/...?token=<jwt>`.
 *
 * The first schemas this plane has had. Screenshot and install predate it and
 * answer bytes or a small ad-hoc JSON body; a directory listing is structured
 * enough that both ends should be reading the same definition, and the provider
 * gets a generated struct to serialise rather than a hand-rolled one.
 *
 * Nothing here is stored anywhere. A pulled file is staged in the provider's
 * scratch directory only for the length of the download that asked for it.
 */

export const FileKind = named("FileKind", z.enum(["file", "directory", "other"]));

export const FileEntry = named(
  "FileEntry",
  z.object({
    name: z.string(),
    /**
     * Absolute on the device, so the browser never joins paths itself. Android
     * and iOS agree on `/` as a separator, but only one of them is answering at
     * a time and neither should have the client guessing on its behalf.
     */
    path: z.string(),
    kind: FileKind,
    /** Absent for a directory, and for anything the backend could not stat. */
    size: z.number().int().optional(),
    modifiedAt: Timestamp.optional(),
  }),
);

export const FileListing = named(
  "FileListing",
  z.object({
    path: z.string(),
    /**
     * `null` at the highest directory this backend will serve, which is what
     * makes the UI hide ".." rather than deciding for itself where the top is.
     */
    parent: z.string().nullable(),
    entries: z.array(FileEntry),
  }),
);

export type FileKind = z.infer<typeof FileKind>;
export type FileEntry = z.infer<typeof FileEntry>;
export type FileListing = z.infer<typeof FileListing>;
