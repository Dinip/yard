import { expect, test } from "bun:test";
import { keyMessage } from "../src/components/device-screen.tsx";

test("Option types resolved characters without forwarding shortcuts or dead keys", () => {
  const optionAt = { key: "@", altKey: true, ctrlKey: false, metaKey: false };
  expect(keyMessage(optionAt, true)).toEqual({ type: "text", text: "@" });
  expect(keyMessage(optionAt, false)).toBeNull();
  expect(keyMessage({ ...optionAt, key: '"', altKey: false }, true)).toEqual({
    type: "text",
    text: '"',
  });
  for (const key of ["Dead", "Alt", "ArrowLeft"]) {
    expect(keyMessage({ ...optionAt, key }, true)).toBeNull();
  }
  expect(keyMessage({ ...optionAt, metaKey: true }, true)).toBeNull();
  expect(keyMessage({ ...optionAt, ctrlKey: true }, true)).toBeNull();
  expect(keyMessage({ ...optionAt, key: "Enter", altKey: false }, true)).toEqual({
    type: "key",
    key: "Enter",
    down: true,
  });
});
