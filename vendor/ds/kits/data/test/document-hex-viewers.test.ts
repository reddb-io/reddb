import { afterEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import DocumentViewer from "../src/DocumentViewer.svelte";
import HexViewer from "../src/HexViewer.svelte";
import { extractDocumentSummary } from "../src/document";
import { formatHex } from "../src/hex";

const documents = [
  {
    id: "incident-42",
    title: "Database latency",
    subtitle: "Open incident",
    body: "The primary database is responding slowly.",
    severity: "high",
  },
  {
    id: "release-7",
    name: "Release notes",
    description: "Version 7",
    content: "The release is ready.",
    author: "Ada",
  },
] as const;

const mounted: Array<{ instance: Record<string, unknown>; target: HTMLElement }> = [];

function render(
  component: Parameters<typeof mount>[0],
  props: Record<string, unknown>,
): HTMLElement {
  const target = document.createElement("div");
  document.body.append(target);
  const instance = mount(component, { target, props }) as Record<string, unknown>;
  mounted.push({ instance, target });
  flushSync();
  return target;
}

afterEach(() => {
  for (const entry of mounted.splice(0)) {
    unmount(entry.instance);
    entry.target.remove();
  }
});

describe("the Document viewer's pure title extraction", () => {
  it("prefers explicit titles and subtitles, then falls back to common document fields", () => {
    expect(extractDocumentSummary(documents[0], 0)).toEqual({
      id: "incident-42",
      title: "Database latency",
      subtitle: "Open incident",
    });
    expect(extractDocumentSummary(documents[1], 1)).toEqual({
      id: "release-7",
      title: "Release notes",
      subtitle: "Version 7",
    });
    expect(extractDocumentSummary({ _id: 19, body: "Only body text" }, 2)).toEqual({
      id: "19",
      title: "Document 3",
      subtitle: "Only body text",
    });
  });
});

describe("the hex viewer's pure formatter", () => {
  it("keeps offset, hex, and ASCII columns aligned for complete and partial rows", () => {
    expect(formatHex([0x00, 0x01, 0x1f, 0x20, 0x41, 0x7e, 0x7f, 0xff], 4)).toEqual([
      { offset: "00000000", hex: "00 01 1f 20", ascii: "... " },
      { offset: "00000004", hex: "41 7e 7f ff", ascii: "A~.." },
    ]);

    const partial = formatHex([0x41, 0x42, 0x43, 0x44, 0x45], 4);
    expect(partial[1]).toEqual({
      offset: "00000004",
      hex: "45         ",
      ascii: "E   ",
    });
    expect(partial.map((row) => [row.offset.length, row.hex.length, row.ascii.length])).toEqual([
      [8, 11, 4],
      [8, 11, 4],
    ]);
  });
});

describe("DocumentViewer", () => {
  it("filters its master list and switches the selected detail between body, fields, and raw", () => {
    const onselect = vi.fn();
    const root = render(DocumentViewer, {
      documents,
      label: "Knowledge base",
      onselect,
    });

    const viewer = root.querySelector<HTMLElement>("[data-document-viewer]")!;
    expect(viewer.getAttribute("aria-label")).toBe("Knowledge base");
    expect(viewer.querySelector("[data-document-body]")?.textContent).toContain(
      "The primary database is responding slowly.",
    );

    const tab = (name: string) =>
      [...viewer.querySelectorAll<HTMLButtonElement>('[role="tab"]')].find((control) => control.textContent?.trim() === name)!;

    tab("Fields").click();
    flushSync();
    expect(viewer.dataset.documentMode).toBe("fields");
    expect([...viewer.querySelectorAll("[data-document-field]")].map((field) => field.textContent)).toContain(
      "severityhigh",
    );

    // The Base Tabs own the keyboard pattern the hand-built tablist lacked.
    tab("Fields").dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowRight", bubbles: true, cancelable: true }));
    flushSync();
    expect(viewer.dataset.documentMode).toBe("raw");
    expect(viewer.querySelector("[data-document-raw]")?.textContent).toContain(
      '"id": "incident-42"',
    );

    const filter = viewer.querySelector<HTMLInputElement>('[aria-label="Filter documents"]')!;
    filter.value = "release";
    filter.dispatchEvent(new Event("input", { bubbles: true }));
    flushSync();
    const options = [...viewer.querySelectorAll<HTMLElement>('[role="option"]')];
    expect(options).toHaveLength(1);
    expect(options[0]?.textContent).toContain("Release notes");

    options[0]!.click();
    flushSync();
    expect(onselect).toHaveBeenLastCalledWith(documents[1], 1);
    expect(viewer.querySelector("[data-document-title]")?.textContent).toBe("Release notes");
  });
});

describe("HexViewer", () => {
  it("pins equally wide text columns even when the final row is incomplete", () => {
    const root = render(HexViewer, {
      value: Uint8Array.from({ length: 18 }, (_, index) => index + 0x20),
      bytesPerRow: 16,
      label: "Packet bytes",
    });
    const viewer = root.querySelector<HTMLElement>("[data-hex-viewer]")!;
    const rows = [...viewer.querySelectorAll<HTMLElement>("[data-hex-row]")];

    expect(viewer.getAttribute("aria-label")).toBe("Packet bytes");
    // A div table: ARIA in HTML forbids role=table on a <section> (wave 5A).
    expect(viewer.tagName).toBe("DIV");
    expect(viewer.getAttribute("role")).toBe("table");
    expect(rows).toHaveLength(2);
    expect(
      rows.map((row) => [
        row.querySelector("[data-hex-offset]")?.textContent?.length,
        row.querySelector("[data-hex-bytes]")?.textContent?.length,
        row.querySelector("[data-hex-ascii]")?.textContent?.length,
      ]),
    ).toEqual([
      [8, 47, 16],
      [8, 47, 16],
    ]);
  });
});

describe("the Document and hex viewer appearance contract", () => {
  it("renders both cleanly across Theme × Color Scheme × Density", () => {
    for (const theme of ["base", "editorial"]) {
      for (const scheme of ["light", "dark"]) {
        for (const density of ["tiny", "compact", "comfortable", "spacious"]) {
          const fixtures = [
            render(DocumentViewer, { documents, label: "Documents" }),
            render(HexViewer, { value: [0, 65, 255], label: "Bytes" }),
          ];
          for (const root of fixtures) {
            root.dataset.theme = theme;
            root.dataset.colorScheme = scheme;
            root.dataset.density = density;
            expect(root.textContent?.trim()).not.toBe("");
            expect(root.textContent).not.toMatch(/undefined|NaN|\[object Object\]/);
            expect(root.querySelector("[data-document-viewer], [data-hex-viewer]")).not.toBeNull();
          }
        }
      }
    }
  });
});
