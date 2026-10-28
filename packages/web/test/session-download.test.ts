import { afterEach, expect, jest, mock, test } from "bun:test";

mock.module("../src/lib/trpc", () => ({
  trpcClient: {
    device: {
      sessionToken: {
        mutate: async () => ({
          token: "fresh-token",
          providerBaseUrl: "https://provider.example",
          sessionUrl: "wss://provider.example/session",
        }),
      },
    },
  },
}));

const { streamDeviceFile } = await import("../src/lib/screen/session");
const originalDocument = Object.getOwnPropertyDescriptor(globalThis, "document");

afterEach(() => {
  jest.useRealTimers();
  if (originalDocument) Object.defineProperty(globalThis, "document", originalDocument);
  else Reflect.deleteProperty(globalThis, "document");
});

test("a download waiting for iOS staging survives past one minute", async () => {
  jest.useFakeTimers();
  let attached = false;
  const frame = {
    hidden: false,
    src: "",
    remove: () => {
      attached = false;
    },
  };
  Object.defineProperty(globalThis, "document", {
    configurable: true,
    value: {
      createElement: (tag: string) => {
        expect(tag).toBe("iframe");
        return frame;
      },
      body: {
        append: (element: unknown) => {
          expect(element).toBe(frame);
          attached = true;
        },
      },
    },
  });

  const path = "drop:/reservation/batch/report.zip";
  await streamDeviceFile("iphone", path);
  const url = new URL(frame.src);
  expect(url.origin).toBe("https://provider.example");
  expect(url.pathname).toBe("/s/iphone/file");
  expect(url.searchParams.get("token")).toBe("fresh-token");
  expect(url.searchParams.get("path")).toBe(path);
  expect(frame.hidden).toBe(true);
  expect(attached).toBe(true);

  jest.advanceTimersByTime(120_000);
  expect(attached).toBe(true);
});
