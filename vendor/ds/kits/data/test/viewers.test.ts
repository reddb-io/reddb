import { afterEach, describe, expect, it, vi } from "vitest";
import { flushSync, mount, unmount } from "svelte";
import Diff from "../src/Diff.svelte";
import JsonTree from "../src/JsonTree.svelte";
import XmlTree from "../src/XmlTree.svelte";
import { diffFields, diffLines } from "../src/diff";
import { buildJsonTree, buildXmlTree } from "../src/viewer-tree";

const mounted: Array<{ instance: Record<string, unknown>; target: HTMLElement }> = [];

function render(component: Parameters<typeof mount>[0], props: Record<string, unknown>): HTMLElement {
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

describe("the Diff viewer's pure model", () => {
  it("reports field additions, removals, and changes with a summary", () => {
    const result = diffFields(
      { name: "before", obsolete: true, stable: 4 },
      { name: "after", created: true, stable: 4 },
    );

    expect(result).toEqual({
      entries: [
        { path: "name", kind: "changed", before: "before", after: "after" },
        { path: "obsolete", kind: "removed", before: true },
        { path: "stable", kind: "unchanged", before: 4, after: 4 },
        { path: "created", kind: "added", after: true },
      ],
      summary: { added: 1, removed: 1, changed: 1, unchanged: 1 },
    });
  });

  it("aligns line insertions without turning later context into changes", () => {
    expect(diffLines("alpha\nbeta\ngamma", "alpha\ninserted\nbeta\nrevised")).toEqual({
      entries: [
        { path: "1", kind: "unchanged", before: "alpha", after: "alpha" },
        { path: "2", kind: "added", after: "inserted" },
        { path: "3", kind: "unchanged", before: "beta", after: "beta" },
        { path: "4", kind: "changed", before: "gamma", after: "revised" },
      ],
      summary: { added: 1, removed: 0, changed: 1, unchanged: 2 },
    });
  });
});

describe("the data viewers' pure dotted-path model", () => {
  it("turns JSON values into TreeView nodes whose ids are their emitted paths", () => {
    expect(buildJsonTree({ user: { names: ["Ada", "Grace"], active: true } })).toEqual([
      {
        id: "user",
        label: "user",
        children: [
          {
            id: "user.names",
            label: "names",
            children: [
              { id: "user.names.0", label: '0: "Ada"', value: "Ada", children: [] },
              { id: "user.names.1", label: '1: "Grace"', value: "Grace", children: [] },
            ],
          },
          { id: "user.active", label: "active: true", value: true, children: [] },
        ],
      },
    ]);
  });

  it("gives repeated XML elements and attributes stable dotted paths", () => {
    const nodes = buildXmlTree(
      '<catalog version="1"><book id="a"><title>One</title></book><book id="b"><title>Two</title></book></catalog>',
    );

    expect(nodes[0]).toMatchObject({
      id: "catalog",
      label: "<catalog>",
      children: [
        { id: "catalog.@version", label: '@version: "1"', value: "1", children: [] },
        {
          id: "catalog.book.0",
          children: [
            { id: "catalog.book.0.@id", label: '@id: "a"', value: "a", children: [] },
            {
              id: "catalog.book.0.title",
              label: '<title>: "One"',
              value: "One",
              children: [],
            },
          ],
        },
        {
          id: "catalog.book.1",
          children: [
            { id: "catalog.book.1.@id", label: '@id: "b"', value: "b", children: [] },
            {
              id: "catalog.book.1.title",
              label: '<title>: "Two"',
              value: "Two",
              children: [],
            },
          ],
        },
      ],
    });
  });
});

describe("JsonTree", () => {
  it("emits a clicked node's dotted path", () => {
    const onselectedchange = vi.fn();
    const root = render(JsonTree, {
      value: { user: { name: "Ada" } },
      expanded: ["user"],
      onselectedchange,
    });

    root.querySelector<HTMLElement>('[data-tree-id="user.name"]')!.click();
    flushSync();

    expect(onselectedchange).toHaveBeenLastCalledWith("user.name");
  });
});

describe("XmlTree", () => {
  it("emits a clicked node's dotted path", () => {
    const onselectedchange = vi.fn();
    const root = render(XmlTree, {
      value: "<response><user><name>Ada</name></user></response>",
      expanded: ["response", "response.user"],
      onselectedchange,
    });

    root.querySelector<HTMLElement>('[data-tree-id="response.user.name"]')!.click();
    flushSync();

    expect(onselectedchange).toHaveBeenLastCalledWith("response.user.name");
  });
});

describe("Diff", () => {
  it("presents field-level change kinds and their summary without relying on colour", () => {
    const root = render(Diff, {
      before: { name: "before", removed: 1 },
      after: { name: "after", added: 2 },
      label: "Record changes",
    });
    const viewer = root.querySelector<HTMLElement>("[data-diff-viewer]")!;
    const entries = [...viewer.querySelectorAll<HTMLElement>("[data-diff-entry]")];

    expect(viewer.getAttribute("aria-label")).toBe("Record changes");
    expect(entries.map((entry) => entry.dataset.changeKind)).toEqual([
      "changed",
      "removed",
      "added",
    ]);
    expect(entries.map((entry) => entry.querySelector("[data-diff-marker]")?.textContent)).toEqual([
      "~",
      "−",
      "+",
    ]);
    expect(viewer.querySelector("[data-diff-summary]")?.textContent).toContain(
      "1 added · 1 removed · 1 changed",
    );
  });
});

describe("the dependency-free viewer appearance contract", () => {
  it("renders JsonTree, XmlTree, and Diff cleanly across Theme × Color Scheme × Density", () => {
    for (const theme of ["base", "editorial"]) {
      for (const scheme of ["light", "dark"]) {
        for (const density of ["tiny", "compact", "comfortable", "spacious"]) {
          const fixtures = [
            render(JsonTree, { value: { status: "ready" }, label: "JSON" }),
            render(XmlTree, { value: "<status>ready</status>", label: "XML" }),
            render(Diff, { before: "before", after: "after", label: "Diff" }),
          ];
          for (const root of fixtures) {
            root.dataset.theme = theme;
            root.dataset.colorScheme = scheme;
            root.dataset.density = density;
            expect(root.textContent?.trim()).not.toBe("");
            expect(root.textContent).not.toMatch(/undefined|NaN|\[object Object\]/);
            expect(root.querySelector('[role="tree"], [data-diff-viewer]')).not.toBeNull();
          }
        }
      }
    }
  });
});
