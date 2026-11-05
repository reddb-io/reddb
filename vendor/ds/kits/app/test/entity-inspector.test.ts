import { mount, unmount, flushSync } from "svelte";
import { act, createElement as h } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import { EntityInspector } from "../react/src/EntityInspector";
import { inspectedEntity } from "../src/core/entity-inspector";
import Fixture from "./fixtures/EntityInspector.svelte";
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
const items = [
  { id: "a", label: "Canary" },
  { id: "b", label: "Migration" },
  { id: "c", label: "Traffic" },
];
it("navigates stable ids, rejects duplicates and treats removed selections as empty", () => {
  expect(inspectedEntity(items, "b")).toMatchObject({
    index: 1,
    previous: "a",
    next: "c",
  });
  expect(inspectedEntity(items, "missing")).toMatchObject({
    index: -1,
    entity: undefined,
    previous: undefined,
    next: undefined,
  });
  expect(() => inspectedEntity([items[0]!, items[0]!], "a")).toThrow();
});
for (const flavor of ["svelte", "react"])
  it(`${flavor} requests navigation without owning selection and hides stale details`, async () => {
    const target = document.createElement("div");
    document.body.append(target);
    const changed = vi.fn(),
      close = vi.fn();
    let options = {
      label: "Inspector",
      items,
      selected: "a" as string | null,
      onselectedchange: changed,
      ondismiss: close,
    };
    let update: () => Promise<void>, dispose: () => Promise<void>;
    if (flavor === "svelte") {
      const app = mount(Fixture, { target, props: { options } });
      flushSync();
      update = async () => {
        flushSync(() => app.update(options));
      };
      dispose = () => unmount(app);
    } else {
      const root = createRoot(target);
      const render = () =>
        h(
          EntityInspector,
          { ...options, actions: h("span", null, "Review controls") },
          h("p", null, "Caller-owned details"),
        );
      await act(() => root.render(render()));
      update = async () => {
        await act(() => root.render(render()));
      };
      dispose = async () => {
        await act(() => root.unmount());
      };
    }
    try {
      expect(target.textContent).toContain("Review controls");
      const button = (label: string) =>
        [...target.querySelectorAll("button")].find(
          (item) => item.textContent?.trim() === label,
        )!;
      expect(button("Previous").disabled).toBe(true);
      await act(() => {
        button("Next").click();
        flushSync();
      });
      expect(changed).toHaveBeenCalledWith("b");
      expect(target.querySelector("h2")?.textContent).toBe("Canary");
      options = { ...options, selected: "c" };
      await update();
      expect(button("Next").disabled).toBe(true);
      await act(() => {
        button("Close").click();
        flushSync();
      });
      expect(close).toHaveBeenCalledOnce();
      options = { ...options, selected: "removed" };
      await update();
      expect(target.textContent).toContain("Select an item to inspect");
      expect(target.textContent).not.toContain("Caller-owned details");
      expect(target.textContent).not.toContain("Review controls");
      expect(button("Previous").disabled).toBe(true);
      expect(button("Next").disabled).toBe(true);
    } finally {
      await dispose();
      target.remove();
    }
  });
