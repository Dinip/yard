import { describe, expect, test } from "bun:test";
import { writeFileSync } from "node:fs";
import { resolve } from "node:path";
import {
  ClientMessage,
  CoordinatorMessage,
  FileListing,
  ProviderMessage,
  ServerMessage,
} from "../src/index.ts";
import {
  clientFixtures,
  controlFixtures,
  coordinatorFixtures,
  fileFixtures,
  serverFixtures,
} from "./fixtures.ts";

const cases = [
  ["ProviderMessage", ProviderMessage, controlFixtures],
  ["CoordinatorMessage", CoordinatorMessage, coordinatorFixtures],
  ["ClientMessage", ClientMessage, clientFixtures],
  ["ServerMessage", ServerMessage, serverFixtures],
  ["FileListing", FileListing, fileFixtures],
] as const;

describe("wire fixtures", () => {
  for (const [name, schema, fixtures] of cases) {
    for (const [label, value] of Object.entries(fixtures)) {
      test(`${name}: ${label} parses and round-trips`, () => {
        const parsed = schema.parse(value);
        // Re-parsing the parse output must be a fixed point: if zod strips or
        // coerces anything, the provider and coordinator disagree about what
        // was actually sent.
        expect(schema.parse(parsed)).toEqual(parsed);
        expect(parsed).toEqual(value);
      });
    }
  }

  test("an unknown discriminator is rejected, not silently ignored", () => {
    expect(() => ProviderMessage.parse({ type: "device.teleport", deviceId: "x" })).toThrow();
  });

  test("a missing required field is rejected", () => {
    expect(() => ClientMessage.parse({ type: "pointer.down", pointerId: 0 })).toThrow();
  });

  /**
   * Writes the fixtures where the Rust test can read them. Generated rather
   * than hand-maintained so the two languages cannot drift apart.
   */
  test("exports fixtures for the Rust side", () => {
    const out = resolve(
      import.meta.dirname,
      "../../provider/crates/yard-protocol/tests/fixtures.json",
    );
    writeFileSync(
      out,
      `${JSON.stringify(
        {
          provider: controlFixtures,
          coordinator: coordinatorFixtures,
          client: clientFixtures,
          server: serverFixtures,
          files: fileFixtures,
        },
        null,
        2,
      )}\n`,
    );
    expect(true).toBe(true);
  });
});
