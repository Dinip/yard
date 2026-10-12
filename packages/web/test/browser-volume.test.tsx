import { expect, test } from "bun:test";
import { renderToStaticMarkup } from "react-dom/server";
import { BrowserVolume } from "../src/components/browser-volume";

function render(available: boolean, warning: string | null = null) {
  return renderToStaticMarkup(
    <BrowserVolume volume={50} available={available} warning={warning} onChange={() => {}} />,
  );
}

test("audio connects beside a disabled slider", () => {
  const html = render(false);
  expect(html).toContain('disabled=""');
  expect(html).toContain("Connecting audio…");
  expect(html).toContain("aria-describedby=");
});

test("audio failures stay beside the disabled slider with details on hover", () => {
  const html = render(false, "Android rejected the media volume change");
  expect(html).toContain('disabled=""');
  expect(html).toContain("Audio unavailable");
  expect(html).toContain('title="Android rejected the media volume change"');
  expect(html).not.toContain("Connecting audio…");
});

test("ready audio enables the slider and clears its status", () => {
  const html = render(true);
  expect(html).not.toContain("disabled=");
  expect(html).not.toContain('role="status"');
  expect(html).not.toContain("aria-describedby=");
});
