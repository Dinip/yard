/**
 * Emits the Rust mirror of the zod schemas in `src/`.
 *
 *   bun run protocol:gen
 *
 * The output is committed, and CI runs `protocol:gen && git diff --exit-code`.
 * That makes impossible the failure stf-ios-provider's README warns about: a
 * stale copy of the wire contract that encodes cleanly and delivers nothing.
 *
 * This walks zod v4's `_zod.def` internals rather than going through JSON
 * Schema, because JSON Schema erases exactly what we need most — discriminated
 * unions become `anyOf` and lose the tag, which is the whole shape of the
 * protocol. If a zod upgrade breaks this walker it will throw on an unhandled
 * node type rather than emit something subtly wrong.
 */
import { mkdirSync, writeFileSync } from "node:fs";
import { dirname, resolve } from "node:path";
import type { z } from "zod";
import {
  AU_DELTA,
  AU_KEY,
  AU_KEY_RESET,
  JWKS_PATH,
  PROTOCOL_VERSION,
  SESSION_TOKEN_AUDIENCE,
} from "../src/index.ts"; // also populates the registry as a side effect
import { registry } from "../src/registry.ts";

const OUT = resolve(import.meta.dirname, "../../provider/crates/farm-protocol/src/generated.rs");

// ── zod introspection ──────────────────────────────────────────────────────

type Def = {
  type: string;
  shape?: Record<string, z.ZodType>;
  element?: z.ZodType;
  innerType?: z.ZodType;
  entries?: Record<string, string>;
  values?: unknown[];
  options?: z.ZodType[];
  discriminator?: string;
  checks?: Array<{ _zod: { def: { format?: string; check?: string } } }>;
  valueType?: z.ZodType;
};

const def = (s: z.ZodType): Def => (s as unknown as { _zod: { def: Def } })._zod.def;

/** Registered schemas, by identity, so a field referencing one emits its name. */
const nameBySchema = new Map<z.ZodType, string>();
for (const [name, schema] of registry) nameBySchema.set(schema, name);

function isIntegerNumber(d: Def): boolean {
  return !!d.checks?.some((c) => {
    const cd = c._zod.def;
    return cd.check === "number_format" && (cd.format ?? "").includes("int");
  });
}

/**
 * Unwraps optional/nullable/default wrappers.
 *
 * `optional` and `nullable` both become `Option<T>` in Rust but must serialize
 * differently, so they are tracked separately:
 *
 *   .optional()  →  the key may be absent   →  skip_serializing_if
 *   .nullable()  →  the key is always sent, possibly as `null`
 *
 * Collapsing the two loses information the wire actually carries — a
 * `clipboard` message with `text: null` means "the clipboard is empty", which
 * is not the same as not reporting the clipboard at all.
 */
function unwrap(schema: z.ZodType): { inner: z.ZodType; optional: boolean; nullable: boolean } {
  let cur = schema;
  let optional = false;
  let nullable = false;
  for (;;) {
    const d = def(cur);
    if (d.type === "optional" || d.type === "default") {
      optional = true;
      cur = d.innerType!;
      continue;
    }
    if (d.type === "nullable") {
      nullable = true;
      cur = d.innerType!;
      continue;
    }
    return { inner: cur, optional, nullable };
  }
}

// ── type mapping ───────────────────────────────────────────────────────────

function rustType(schema: z.ZodType, path: string): string {
  const registered = nameBySchema.get(schema);
  if (registered) return registered;

  const d = def(schema);
  switch (d.type) {
    case "string":
      return "String";
    case "number":
      return isIntegerNumber(d) ? "i64" : "f64";
    case "boolean":
      return "bool";
    case "array":
      return `Vec<${rustType(d.element!, `${path}[]`)}>`;
    case "record":
      return "serde_json::Map<String, serde_json::Value>";
    case "any":
    case "unknown":
      return "serde_json::Value";
    case "literal":
      // Only meaningful as a discriminator, which is stripped before we get here.
      if (typeof d.values?.[0] === "string") return "String";
      throw new Error(`${path}: non-string literal cannot be mapped`);
    case "object":
      throw new Error(
        `${path}: nested object is not registered. Wrap it with named("Foo", …) ` +
          `in the protocol package so it has a Rust type name.`,
      );
    case "enum":
      throw new Error(`${path}: enum is not registered. Wrap it with named("Foo", …).`);
    default:
      throw new Error(`${path}: unhandled zod type "${d.type}"`);
  }
}

// ── naming ─────────────────────────────────────────────────────────────────

const snake = (s: string) => s.replace(/([a-z0-9])([A-Z])/g, "$1_$2").toLowerCase();

const pascal = (s: string) =>
  s
    .split(/[._\-\s]+/)
    .filter(Boolean)
    .map((p) => p.charAt(0).toUpperCase() + p.slice(1))
    .join("");

/** `type` is a Rust keyword; a handful of field names need escaping. */
const RUST_KEYWORDS = new Set(["type", "ref", "match", "move", "box", "fn", "mod", "self"]);
const fieldName = (s: string) => {
  const n = snake(s);
  return RUST_KEYWORDS.has(n) ? `r#${n}` : n;
};

// ── emitters ───────────────────────────────────────────────────────────────

const camel = (s: string) => s.replace(/_([a-z0-9])/g, (_, c: string) => c.toUpperCase());

/**
 * `vis` is empty for enum struct-variants: `pub` is not valid there, since a
 * variant's fields inherit the enum's own visibility.
 */
function emitStructFields(
  shape: Record<string, z.ZodType>,
  path: string,
  indent = "    ",
  vis = "pub ",
): string {
  const lines: string[] = [];
  for (const [key, value] of Object.entries(shape)) {
    const { inner, optional, nullable } = unwrap(value);
    const ty = rustType(inner, `${path}.${key}`);
    const rustName = fieldName(key);
    const wrapped = optional || nullable;

    const attrs: string[] = [];
    // `rename_all = "camelCase"` on the container already covers the common
    // case; only spell out a rename when that round-trip would not reproduce
    // the wire name exactly (e.g. "osVersion" vs an acronym-heavy key).
    if (camel(rustName.replace(/^r#/, "")) !== key) attrs.push(`rename = "${key}"`);
    // Only `.optional()` may vanish from the JSON. A `.nullable()` field is
    // always present, as `null` — see unwrap().
    if (optional) attrs.push('skip_serializing_if = "Option::is_none"', "default");
    else if (nullable) attrs.push("default");

    if (attrs.length) lines.push(`${indent}#[serde(${attrs.join(", ")})]`);
    lines.push(`${indent}${vis}${rustName}: ${wrapped ? `Option<${ty}>` : ty},`);
  }
  return lines.join("\n");
}

function emitEnum(name: string, d: Def): string {
  const variants = Object.values(d.entries ?? {});
  const lines = [
    "#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]",
    `pub enum ${name} {`,
  ];
  for (const v of variants) {
    lines.push(`    #[serde(rename = "${v}")]`);
    lines.push(`    ${pascal(v)},`);
  }
  lines.push("}");
  return lines.join("\n");
}

function emitStruct(name: string, d: Def): string {
  return [
    "#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]",
    '#[serde(rename_all = "camelCase")]',
    `pub struct ${name} {`,
    emitStructFields(d.shape!, name),
    "}",
  ].join("\n");
}

/**
 * A discriminated union becomes an internally-tagged enum. The discriminator
 * field is removed from each variant's fields — serde reconstructs it from the
 * variant name — so the JSON on the wire is byte-identical to what zod parses.
 */
function emitTaggedUnion(name: string, d: Def): string {
  const tag = d.discriminator!;
  const lines = [
    // A wire union's variants are whatever the protocol says they are: `hello`
    // carries a whole inventory and `heartbeat` carries a timestamp. Boxing the
    // big one to even them out would change nothing on the wire and cost every
    // match site a deref.
    "#[allow(clippy::large_enum_variant)]",
    "#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]",
    `#[serde(tag = "${tag}", rename_all = "camelCase")]`,
    `pub enum ${name} {`,
  ];

  for (const option of d.options ?? []) {
    const od = def(option);
    if (od.type !== "object") throw new Error(`${name}: union option is not an object`);

    const shape = { ...od.shape! };
    const tagSchema = shape[tag];
    if (!tagSchema) throw new Error(`${name}: union option is missing discriminator "${tag}"`);
    const tagValue = def(tagSchema).values?.[0];
    if (typeof tagValue !== "string") {
      throw new Error(`${name}: discriminator "${tag}" is not a string literal`);
    }
    delete shape[tag];

    lines.push(`    #[serde(rename = "${tagValue}", rename_all = "camelCase")]`);
    if (Object.keys(shape).length === 0) {
      lines.push(`    ${pascal(tagValue)},`);
    } else {
      lines.push(`    ${pascal(tagValue)} {`);
      lines.push(emitStructFields(shape, `${name}::${tagValue}`, "        ", ""));
      lines.push("    },");
    }
  }

  lines.push("}");
  return lines.join("\n");
}

// ── main ───────────────────────────────────────────────────────────────────

const blocks: string[] = [];
for (const [name, schema] of registry) {
  const d = def(schema);
  switch (d.type) {
    case "enum":
      blocks.push(emitEnum(name, d));
      break;
    case "object":
      blocks.push(emitStruct(name, d));
      break;
    case "union":
      if (!d.discriminator) throw new Error(`${name}: only discriminated unions are supported`);
      blocks.push(emitTaggedUnion(name, d));
      break;
    default:
      throw new Error(`${name}: cannot emit a top-level "${d.type}"`);
  }
}

// Interpolated from the TypeScript values rather than restated, so a change on
// one side cannot leave the other quietly disagreeing.
const header = `// @generated by packages/protocol/scripts/gen-rust.ts — DO NOT EDIT.
//
// Regenerate with \`bun run protocol:gen\`. CI fails if this file is out of date
// with the zod schemas in packages/protocol/src, so a wire-contract change can
// never reach a provider as a silent no-op.

use serde::{Deserialize, Serialize};

/// Binary video frame type byte, sent as the first byte of each session-plane
/// binary frame.
pub const AU_KEY: u8 = ${AU_KEY};
pub const AU_DELTA: u8 = ${AU_DELTA};
pub const AU_KEY_RESET: u8 = ${AU_KEY_RESET};

pub const PROTOCOL_VERSION: i64 = ${PROTOCOL_VERSION};

/// Where the coordinator publishes the Ed25519 key session tokens are signed
/// with. Each provider fetches this once at startup and caches it.
pub const JWKS_PATH: &str = ${JSON.stringify(JWKS_PATH)};

/// Session tokens must carry this audience; anything else is rejected.
pub const SESSION_TOKEN_AUDIENCE: &str = ${JSON.stringify(SESSION_TOKEN_AUDIENCE)};
`;

mkdirSync(dirname(OUT), { recursive: true });
writeFileSync(OUT, `${header}\n${blocks.join("\n\n")}\n`);

/**
 * Normalise through rustfmt so `cargo fmt --check` and
 * `protocol:gen && git diff --exit-code` cannot disagree about this file. Two
 * CI checks that each demand a different byte sequence would make the build
 * unpassable, and the emitter is not the right place to reimplement rustfmt's
 * layout rules.
 */
const fmt = Bun.spawnSync(["rustfmt", "--edition", "2021", OUT]);
if (fmt.exitCode !== 0) {
  const detail = new TextDecoder().decode(fmt.stderr).trim();
  console.warn(
    `[protocol] rustfmt failed (${detail || "not installed"}) — output left unformatted, ` +
      "which will fail `cargo fmt --check`. Install it with `rustup component add rustfmt`.",
  );
}

console.log(`[protocol] wrote ${blocks.length} types → ${OUT}`);
