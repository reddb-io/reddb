import { mount, unmount, flushSync } from "svelte";
import { act, createElement as h } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi, afterEach } from "vitest";
import { SceneViewport } from "../react/src/SceneViewport";
import {
  projectAnnotation,
  sceneAnnotations,
  mountSceneAnnotations,
} from "../src/scene-annotations";
import Fixture from "./fixtures/SceneFollow.svelte";
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
afterEach(() => vi.restoreAllMocks());
it("projects logical anchors at fixed label size, clamps edges and omits off-scene anchors", () => {
  const box = { width: 320, height: 200 },
    label = { width: 120, height: 44 };
  expect(
    projectAnnotation({ x: 20, y: 20 }, { x: 0, y: 0, scale: 1 }, box, label),
  ).toEqual({ x: 0, y: 0, visible: true });
  expect(
    projectAnnotation({ x: 100, y: 80 }, { x: 0, y: 0, scale: 2 }, box, label),
  ).toEqual({ x: 140, y: 116, visible: true });
  expect(
    projectAnnotation({ x: 400, y: 80 }, { x: 0, y: 0, scale: 2 }, box, label)
      .visible,
  ).toBe(false);
  expect(() =>
    sceneAnnotations([{ id: "a", label: "A", x: NaN, y: 2 }]),
  ).toThrow();
  expect(() =>
    sceneAnnotations([
      { id: "a", label: "A", x: 1, y: 2 },
      { id: "a", label: "A", x: 3, y: 4 },
    ]),
  ).toThrow();
});
for (const flavor of ["react", "svelte"])
  for (const reduced of [false, true])
    it(`${flavor} reduced=${reduced} separates selection, follow navigation and controlled suspension`, async () => {
      vi.spyOn(window, "matchMedia").mockImplementation((query) => ({
        matches: reduced && query.includes("prefers-reduced-motion"),
        media: query,
        addListener() {},
        removeListener() {},
        addEventListener() {},
        removeEventListener() {},
        onchange: null,
        dispatchEvent() {
          return true;
        },
      }));
      vi.spyOn(HTMLElement.prototype, "getBoundingClientRect").mockReturnValue({
        x: 0,
        y: 0,
        left: 0,
        top: 0,
        right: 600,
        bottom: 300,
        width: 600,
        height: 300,
        toJSON() {},
      });
      vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockReturnValue(
        600,
      );
      vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(
        300,
      );
      const target = document.createElement("div");
      document.body.append(target);
      const selection = vi.fn(),
        follow = vi.fn();
      let options: any = {
        label: "Service map",
        contentWidth: 1000,
        contentHeight: 400,
        annotations: [{ id: "a", label: "Gateway", x: 120, y: 200 }],
        selected: "a",
        onannotationselect: selection,
        follow: { x: 0, y: 100, width: 400, height: 200 },
        following: reduced,
        onfollowingchange: follow,
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
        await act(() =>
          root.render(
            h(SceneViewport, options, h("p", null, "Static fallback")),
          ),
        );
        update = async () => {
          await act(() =>
            root.render(
              h(SceneViewport, options, h("p", null, "Static fallback")),
            ),
          );
        };
        dispose = async () => {
          await act(() => root.unmount());
        };
      }
      try {
        if (reduced) {
          expect(follow).toHaveBeenLastCalledWith(false, "reduced-motion");
          expect(
            [...target.querySelectorAll("button")].find(
              (node) =>
                node.textContent?.trim() === "Follow" ||
                node.textContent?.trim() === "Pause following",
            )?.disabled,
          ).toBe(true);
          return;
        }
        const button = (text: string) =>
          [...target.querySelectorAll("button")].find(
            (node) => node.textContent?.trim() === text,
          )!;
        await act(() => {
          target
            .querySelector<HTMLButtonElement>("[data-scene-annotation]")!
            .click();
          flushSync();
        });
        expect(selection).toHaveBeenCalledWith("a");
        expect(follow).not.toHaveBeenCalled();
        await act(() => {
          button("Follow").click();
          flushSync();
        });
        expect(follow).toHaveBeenLastCalledWith(true, "control");
        options = { ...options, following: true };
        await update();
        const viewport = target.querySelector<HTMLElement>("[role=region]")!,
          drawing = target.querySelector<HTMLElement>(
            "[data-pan-zoom-content]",
          )!;
        const before = drawing.style.transform;
        await act(() => {
          button("Zoom in").click();
          flushSync();
        });
        expect(follow).toHaveBeenLastCalledWith(false, "navigate");
        options = { ...options, following: false };
        await update();
        const paused = drawing.style.transform;
        expect(paused).not.toBe(before);
        options = { ...options, follow: { ...options.follow, x: 400 } };
        await update();
        expect(drawing.style.transform).toBe(paused);
        options = { ...options, following: true };
        await update();
        expect(drawing.style.transform).not.toBe(paused);
        await act(() => {
          button("Fit").click();
          flushSync();
        });
        expect(follow).toHaveBeenLastCalledWith(false, "navigate");
        options = { ...options, follow: null };
        await update();
        expect(follow).toHaveBeenLastCalledWith(false, "unavailable");
        expect(
          button("Follow")?.disabled ?? button("Pause following").disabled,
        ).toBe(true);
        expect(target.textContent).toContain("Static fallback");
      } finally {
        await dispose();
        target.remove();
      }
    });

it("prioritizes selection and focus when labels collide, removes off-scene targets and releases observers", () => {
  const host = document.createElement("div");
  host.innerHTML =
    '<div data-pan-zoom><div role="region"><button data-scene-annotation="a" data-x="100" data-y="100">A</button><button data-scene-annotation="b" data-x="110" data-y="100" data-selected="true">B</button><button data-scene-annotation="c" data-x="1000" data-y="100">C</button></div></div>';
  document.body.append(host);
  vi.spyOn(HTMLElement.prototype, "clientWidth", "get").mockReturnValue(320);
  vi.spyOn(HTMLElement.prototype, "clientHeight", "get").mockReturnValue(200);
  vi.spyOn(HTMLElement.prototype, "offsetWidth", "get").mockReturnValue(100);
  vi.spyOn(HTMLElement.prototype, "offsetHeight", "get").mockReturnValue(40);
  const controller = mountSceneAnnotations(host),
    [a, b, c] = [...host.querySelectorAll<HTMLButtonElement>("button")];
  try {
    controller.update({ x: 0, y: 0, scale: 1 });
    expect(a!.hidden).toBe(true);
    expect(b!.hidden).toBe(false);
    expect(c!.hidden).toBe(true);
    a!.hidden = false;
    a!.focus();
    controller.update({ x: 0, y: 0, scale: 1 });
    expect(a!.hidden).toBe(false);
    expect(b!.hidden).toBe(true);
    controller.update({ x: -800, y: 0, scale: 1 });
    expect(a!.hidden).toBe(false);
    expect(c!.hidden).toBe(false);
  } finally {
    controller.destroy();
    host.remove();
  }
});
