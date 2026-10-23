import { mount, unmount, flushSync } from "svelte";
import { act, createElement as h } from "react";
import { createRoot } from "react-dom/client";
import { renderToString } from "react-dom/server";
import { expect, it, vi } from "vitest";
import StatusBreakdown from "../react/src/StatusBreakdown";
import Fixture from "./fixtures/StatusBreakdown.svelte";
import { statusCounts } from "../src/core/status-breakdown";
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
const items = [
  { id: "done", label: "Completed", count: 8, tone: "success" as const },
  { id: "blocked", label: "Blocked", count: 2, tone: "warning" as const },
  { id: "queued", label: "Queued", count: 0 },
];
it("rejects invalid exact counts and duplicate state identities", () => {
  for (const count of [-1, 0.5, NaN, Infinity])
    expect(() => statusCounts([{ ...items[0]!, count }])).toThrow();
  expect(() => statusCounts([items[0]!, items[0]!])).toThrow();
  expect(statusCounts(items).total).toBe(10);
  expect(items.map((item) => item.count)).toEqual([8, 2, 0]);
});
it("renders exact textual state counts on the server without engines or appearance axes", () => {
  const html = renderToString(h(StatusBreakdown, { label: "Work", items }));
  expect(html).toContain("Completed");
  expect(html).toContain("10");
  expect(html).not.toMatch(/data-(theme|density|color-scheme)=/);
});
for (const flavor of ["svelte", "react"])
  it(`${flavor} preserves counts, zero states and controlled filter selection`, async () => {
    const target = document.createElement("div");
    document.body.append(target);
    const selected = vi.fn();
    let options = {
      label: "Work",
      items,
      selected: "blocked" as string | null,
      interactive: true,
      onselectedchange: selected,
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
      await act(() => root.render(h(StatusBreakdown, options)));
      update = async () => {
        await act(() => root.render(h(StatusBreakdown, options)));
      };
      dispose = async () => {
        await act(() => root.unmount());
      };
    }
    try {
      expect(
        [...target.querySelectorAll<HTMLElement>("[data-status-segment]")].map(
          (item) => item.style.width,
        ),
      ).toEqual(["80%", "20%"]);
      const blocked = target.querySelector<HTMLButtonElement>(
        '[data-status-filter="blocked"]',
      )!;
      await act(() => {
        blocked.click();
        flushSync();
      });
      expect(selected).toHaveBeenLastCalledWith(null);
      expect(blocked.getAttribute("aria-pressed")).toBe("true");
      const zero = target.querySelector<HTMLButtonElement>(
        '[data-status-filter="queued"]',
      )!;
      await act(() => {
        zero.click();
        flushSync();
      });
      expect(selected).toHaveBeenLastCalledWith("queued");
      options = {
        ...options,
        items: items.map((item) => ({ ...item, count: 0 })),
        selected: null,
      };
      await update();
      expect(target.querySelectorAll("[data-status-segment]")).toHaveLength(0);
      expect(target.textContent).toContain("No items");
      options = { ...options, interactive: false };
      await update();
      expect(target.querySelector("button")).toBeNull();
    } finally {
      await dispose();
      target.remove();
    }
  });
