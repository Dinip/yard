import changelog from "../../../../CHANGELOG.md?raw";
import { VERSION } from "./build-info";
import { parseChangelog } from "./changelog";

export const releases = parseChangelog(changelog);
export const currentRelease = releases.find((release) => release.version === VERSION);
