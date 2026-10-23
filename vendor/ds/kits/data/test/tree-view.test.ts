import { afterEach, describe, expect, it, vi } from "vitest";
import { createRawSnippet, flushSync, mount, tick, unmount } from "svelte";
import TreeView from "../src/TreeView.svelte";
import { buildPathTree } from "../src/tree";

const nodes = [
  {
    id: "projects",
    label: "Projects",
    children: [
      { id: "projects/index", label: "index.ts", children: [] },
      { id: "projects/utils", label: "utils.ts", children: [] },
    ],
  },
  { id: "readme", label: "README.md", children: [] },
] as const;

const mounted: Array<{ instance: Record<string, unknown>; target: HTMLElement }> = [];

function render(props: Record<string, unknown>): HTMLElement {
  const target = document.createElement("div");
  document.body.append(target);
  const instance = mount(TreeView, { target, props }) as Record<string, unknown>;
  mounted.push({ instance, target });
  flushSync();
  return target;
}

function press(element: Element, key: string): KeyboardEvent {
  const event = new KeyboardEvent("keydown", { key, bubbles: true, cancelable: true });
  element.dispatchEvent(event);
  flushSync();
  return event;
}

afterEach(() => {
  vi.useRealTimers();
  for (const entry of mounted.splice(0)) {
    unmount(entry.instance);
    entry.target.remove();
  }
});

describe("the path-to-tree builder", () => {
  it("splits path keys into naturally sorted nodes while preserving generic leaf values", () => {
    const files = {
      "src/components/Button10.svelte": { bytes: 10 },
      "README.md": { bytes: 1 },
      "src//components/Button2.svelte/": { bytes: 2 },
      "src/index.ts": { bytes: 3 },
    } as const;

    expect(buildPathTree(files)).toEqual([
      {
        id: "README.md",
        label: "README.md",
        value: files["README.md"],
        children: [],
      },
      {
        id: "src",
        label: "src",
        children: [
          {
            id: "src/components",
            label: "components",
            children: [
              {
                id: "src/components/Button2.svelte",
                label: "Button2.svelte",
                value: files["src//components/Button2.svelte/"],
                children: [],
              },
              {
                id: "src/components/Button10.svelte",
                label: "Button10.svelte",
                value: files["src/components/Button10.svelte"],
                children: [],
              },
            ],
          },
          {
            id: "src/index.ts",
            label: "index.ts",
            value: files["src/index.ts"],
            children: [],
          },
        ],
      },
    ]);
    expect(Object.keys(files)).toEqual([
      "src/components/Button10.svelte",
      "README.md",
      "src//components/Button2.svelte/",
      "src/index.ts",
    ]);
  });
});

describe("the Data Kit TreeView", () => {
  it("follows the WAI-ARIA tree keyboard pattern across visible nodes", () => {
    const root = render({ label: "Project files", nodes });
    const tree = root.querySelector<HTMLElement>('[role="tree"]')!;
    const projects = root.querySelector<HTMLElement>('[data-tree-id="projects"]')!;

    expect(tree.getAttribute("aria-label")).toBe("Project files");
    expect(projects.getAttribute("role")).toBe("treeitem");
    expect(projects.getAttribute("aria-level")).toBe("1");
    expect(projects.getAttribute("aria-expanded")).toBe("false");
    expect(projects.tabIndex).toBe(0);

    projects.focus();
    expect(press(projects, "ArrowRight").defaultPrevented).toBe(true);
    expect(projects.getAttribute("aria-expanded")).toBe("true");
    press(projects, "ArrowRight");
    const index = root.querySelector<HTMLElement>('[data-tree-id="projects/index"]')!;
    const utils = root.querySelector<HTMLElement>('[data-tree-id="projects/utils"]')!;
    expect(document.activeElement).toBe(index);
    expect(index.getAttribute("aria-level")).toBe("2");

    press(index, "ArrowDown");
    expect(document.activeElement).toBe(utils);
    press(utils, "ArrowUp");
    expect(document.activeElement).toBe(index);
    press(index, "u");
    expect(document.activeElement).toBe(utils);
    press(utils, "ArrowLeft");
    expect(document.activeElement).toBe(projects);
    press(projects, "ArrowLeft");
    expect(projects.getAttribute("aria-expanded")).toBe("false");
  });

  it("emits expansion and selection changes without overriding controlled state", () => {
    const onexpandedchange = vi.fn();
    const onselectedchange = vi.fn();
    const root = render({
      label: "Project files",
      nodes,
      expanded: ["projects"],
      selected: "projects/index",
      onexpandedchange,
      onselectedchange,
    });
    const projects = root.querySelector<HTMLElement>('[data-tree-id="projects"]')!;
    const index = root.querySelector<HTMLElement>('[data-tree-id="projects/index"]')!;
    const utils = root.querySelector<HTMLElement>('[data-tree-id="projects/utils"]')!;

    expect(projects.getAttribute("aria-expanded")).toBe("true");
    expect(index.getAttribute("aria-selected")).toBe("true");
    press(projects, "ArrowLeft");
    expect(onexpandedchange).toHaveBeenLastCalledWith([]);
    expect(projects.getAttribute("aria-expanded")).toBe("true");

    press(utils, " ");
    expect(onselectedchange).toHaveBeenLastCalledWith("projects/utils");
    expect(index.getAttribute("aria-selected")).toBe("true");
    expect(utils.getAttribute("aria-selected")).toBe("false");
  });

  it("offers an optional rename action without mutating the caller's node", async () => {
    const onrename = vi.fn();
    const root = render({ label: "Project files", nodes, onrename });
    const projects = root.querySelector<HTMLElement>('[data-tree-id="projects"]')!;

    expect(root.querySelector('[aria-label="Rename Projects"]')).not.toBeNull();
    expect(press(projects, "F2").defaultPrevented).toBe(true);
    await tick();
    const editor = root.querySelector<HTMLInputElement>('[data-tree-rename="projects"]')!;
    expect(document.activeElement).toBe(editor);
    editor.value = "Workspace";
    editor.dispatchEvent(new Event("input", { bubbles: true }));
    press(editor, "Enter");

    expect(onrename).toHaveBeenLastCalledWith(nodes[0], "Workspace");
    expect(nodes[0].label).toBe("Projects");
    expect(root.querySelector('[data-tree-rename="projects"]')).toBeNull();
  });

  it("reports an optional drag-and-drop reorder without mutating caller nodes", () => {
    const onreorder = vi.fn();
    const root = render({
      label: "Project files",
      nodes,
      expanded: ["projects"],
      onreorder,
    });
    const index = root.querySelector<HTMLElement>('[data-tree-id="projects/index"]')!;
    const utils = root.querySelector<HTMLElement>('[data-tree-id="projects/utils"]')!;
    const dataTransfer = {
      effectAllowed: "none",
      dropEffect: "none",
      setData: vi.fn(),
      getData: vi.fn(() => "projects/index"),
    };
    const dragstart = new Event("dragstart", { bubbles: true, cancelable: true });
    const dragover = new Event("dragover", { bubbles: true, cancelable: true });
    const drop = new Event("drop", { bubbles: true, cancelable: true });
    for (const event of [dragstart, dragover, drop]) {
      Object.defineProperty(event, "dataTransfer", { value: dataTransfer });
    }

    expect(index.draggable).toBe(true);
    index.dispatchEvent(dragstart);
    utils.dispatchEvent(dragover);
    utils.dispatchEvent(drop);

    expect(dragover.defaultPrevented).toBe(true);
    expect(onreorder).toHaveBeenLastCalledWith({
      sourceId: "projects/index",
      targetId: "projects/utils",
      position: "after",
    });
    expect(nodes[0].children.map((node) => node.id)).toEqual([
      "projects/index",
      "projects/utils",
    ]);
  });

  it("renders cleanly across Theme × Color Scheme × Density", () => {
    for (const theme of ["base", "editorial"]) {
      for (const scheme of ["light", "dark"]) {
        for (const density of ["tiny", "compact", "comfortable", "spacious"]) {
          const root = render({ label: `${theme}-${scheme}-${density}`, nodes, expanded: ["projects"] });
          root.dataset.theme = theme;
          root.dataset.colorScheme = scheme;
          root.dataset.density = density;
          const tree = root.querySelector<HTMLElement>("[data-tree-view]")!;

          expect(tree.querySelectorAll('[role="treeitem"]')).toHaveLength(4);
          expect(tree.hasAttribute("data-theme")).toBe(false);
          expect(tree.hasAttribute("data-color-scheme")).toBe(false);
          expect(tree.hasAttribute("data-density")).toBe(false);
        }
      }
    }
  });

  it("draws the selected node with the selection state and hangs caller decoration off each node", () => {
    const leading = createRawSnippet((node: () => { id: string }) => ({
      render: () => `<i data-kind="${node().id}"></i>`,
    }));
    const trailing = createRawSnippet((node: () => { children: readonly unknown[] }) => ({
      render: () => `<b>${node().children.length}</b>`,
    }));
    const root = render({ label: "Files", nodes, selected: "readme", leading, trailing });
    const items = [...root.querySelectorAll<HTMLElement>('[role="treeitem"]')];
    const readme = items.find((item) => item.dataset.treeId === "readme")!;

    expect(readme.getAttribute("aria-selected")).toBe("true");
    expect(readme.className).toContain("aria-selected:bg-foreground/10");
    expect(readme.className).toContain("hover:bg-foreground/8");
    expect(readme.querySelector("[data-tree-leading] [data-kind='readme']")).not.toBeNull();
    expect(items.map((item) => item.querySelector("[data-tree-trailing]")?.textContent)).toEqual([
      "2",
      "0",
    ]);
  });
});
