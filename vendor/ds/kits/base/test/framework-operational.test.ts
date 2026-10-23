import { mount, unmount, flushSync } from "svelte";
import { act, createElement as h, type ComponentType } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, expect, it, vi } from "vitest";
import * as ReactKit from "../react/src";
import Fixture from "./fixtures/Operational.svelte";
import { segmentGeometry } from "../src/core/segments";
import {
  valuePresentation,
  transferPresentation,
} from "../src/core/operational-motion";
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
afterEach(() => {
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
  document.body.replaceChildren();
});
it("bounds decorative segments without changing the exact scalar or partial fill", () => {
  expect(segmentGeometry(4, 37.5).fills).toEqual([100, 50, 0, 0]);
  expect(segmentGeometry(10000, 100, 8)).toMatchObject({
    columns: 8,
    aggregated: true,
  });
  expect(segmentGeometry(10000, 100).fills).toHaveLength(128);
  expect(() => segmentGeometry(Infinity, 20)).toThrow();
});
for (const flavor of ["svelte", "react"])
  for (const name of ["Progress", "Meter", "Statistic"] as const)
    it(`${flavor} ${name} preserves real readings and segmented semantics`, async () => {
      const target = document.createElement("div");
      document.body.append(target);
      const options = {
        label: "Energy",
        value: 37.5,
        ...(name === "Statistic"
          ? { animate: true }
          : { segments: 4, segmentColumns: 2 }),
      };
      let dispose: () => Promise<void>;
      if (flavor === "svelte") {
        const app = mount(Fixture, { target, props: { name, options } });
        flushSync();
        dispose = () => unmount(app);
      } else {
        const root = createRoot(target);
        await act(() =>
          root.render(h(ReactKit[name] as ComponentType<any>, options)),
        );
        dispose = async () => {
          await act(() => root.unmount());
        };
      }
      try {
        if (name === "Statistic") {
          expect(target.querySelector("data")?.getAttribute("value")).toBe(
            "37.5",
          );
        } else {
          const root = target.querySelector("[role]")!;
          expect(root.getAttribute("role")).toBe(
            name === "Meter" ? "meter" : "progressbar",
          );
          expect(root.getAttribute("aria-valuenow")).toBe("37.5");
          expect(target.querySelectorAll("[data-segment-fill]")).toHaveLength(
            4,
          );
          expect(
            target
              .querySelectorAll("[data-segment-fill]")[1]
              ?.getAttribute("style"),
          ).toContain("50%");
        }
      } finally {
        await dispose();
      }
    });
it("removes all transfer identifiers and cancels clones without changing source/destination state", () => {
  const host = document.createElement("span"),
    from = document.createElement("div"),
    to = document.createElement("div");
  from.id = "source";
  to.id = "destination";
  from.textContent = "Committed";
  to.textContent = "Received";
  document.body.append(host, from, to);
  const rect = {
    x: 0,
    y: 0,
    left: 0,
    top: 0,
    right: 40,
    bottom: 40,
    width: 40,
    height: 40,
    toJSON() {
      return {};
    },
  };
  vi.spyOn(from, "getBoundingClientRect").mockReturnValue(rect);
  vi.spyOn(to, "getBoundingClientRect").mockReturnValue({ ...rect, left: 100 });
  const cancel = vi.fn();
  vi.stubGlobal("Animation", class {});
  Object.defineProperty(HTMLElement.prototype, "animate", {
    configurable: true,
    value: () => ({ cancel, finished: new Promise(() => {}) }),
  });
  const stop = transferPresentation(host, "source", "destination", 400);
  const clone = document.querySelector("[data-transfer-clone]");
  expect(clone?.hasAttribute("id")).toBe(false);
  expect(clone?.getAttribute("aria-hidden")).toBe("true");
  stop();
  expect(document.querySelector("[data-transfer-clone]")).toBeNull();
  expect(from.textContent).toBe("Committed");
  expect(to.textContent).toBe("Received");
  delete (HTMLElement.prototype as any).animate;
});
it("updates value paint without interpolating, and cleans its native animation", () => {
  const element = document.createElement("data");
  document.body.append(element);
  const cancel = vi.fn(),
    animate = vi.fn(() => ({ cancel }));
  Object.defineProperty(element, "animate", { value: animate });
  const p = valuePresentation(element);
  p.update(1, true);
  expect(animate).not.toHaveBeenCalled();
  p.update(9, true);
  expect(animate).toHaveBeenCalledTimes(1);
  p.destroy();
  expect(cancel).toHaveBeenCalled();
});

it("cancels value feedback immediately when the document becomes hidden", () => {
  const element = document.createElement("data");
  document.body.append(element);
  const cancel = vi.fn();
  Object.defineProperty(element, "animate", {
    value: vi.fn(() => ({ cancel })),
  });
  const p = valuePresentation(element);
  p.update(1, true);
  p.update(2, true);
  cancel.mockClear();
  vi.spyOn(document, "hidden", "get").mockReturnValue(true);
  document.dispatchEvent(new Event("visibilitychange"));
  expect(cancel).toHaveBeenCalledOnce();
  p.destroy();
});
it("does not create transfer clones under reduced motion", () => {
  const from = document.createElement("div"),
    to = document.createElement("div"),
    host = document.createElement("span");
  from.id = "source";
  to.id = "destination";
  document.body.append(from, to, host);
  vi.stubGlobal(
    "matchMedia",
    vi.fn(() => ({ matches: true }) as MediaQueryList),
  );
  const stop = transferPresentation(host, "source", "destination", 400);
  expect(document.querySelector("[data-transfer-clone]")).toBeNull();
  stop();
});
