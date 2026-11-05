import { mount, unmount, flushSync } from "svelte";
import { act, createElement as h, type ComponentType } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import { StateGrid } from "../react/src/StateGrid";
import { EventLog } from "../react/src/EventLog";
import { QueueView } from "../react/src/QueueView";
import { IsometricBlock } from "../react/src/IsometricBlock";
import { ObjectStack } from "../react/src/ObjectStack";
import { IsometricScene } from "../react/src/IsometricScene";
import { SceneViewport } from "../react/src/SceneViewport";
import { WorkflowDiagram } from "../react/src/WorkflowDiagram";
import { MetricStrip } from "../react/src/MetricStrip";
import Fixture from "./fixtures/Operational.svelte";
import { solidBox, stackItems, sceneGeometry } from "../src/isometric";
import { workflowLayout } from "../src/workflow";
import { retainedEvents, followEvents } from "../src/operational";
import { mountSceneEngine } from "../src/scene-viewport";
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
const adapters = {
  StateGrid,
  EventLog,
  QueueView,
  IsometricBlock,
  ObjectStack,
  IsometricScene,
  SceneViewport,
  WorkflowDiagram,
  MetricStrip,
};
const cell = { id: "J01", label: "Job 1", state: "Waiting" };
const cases = {
  StateGrid: {
    label: "Jobs",
    items: [cell],
    interactive: true,
    selected: "J01",
  },
  QueueView: {
    label: "Queue",
    slots: [
      { id: "s1", label: "Slot 1", item: cell },
      { id: "s2", label: "Slot 2" },
    ],
    interactive: true,
  },
  IsometricBlock: { label: "Solid" },
  ObjectStack: { label: "Stock", count: 100, maxVisible: 4 },
  IsometricScene: {
    label: "Floor",
    items: [{ id: "J01", label: "Job 1", x: 0, y: 0 }],
    interactive: true,
  },
  SceneViewport: { label: "Viewport", contentWidth: 400, contentHeight: 300 },
  WorkflowDiagram: {
    label: "Workflow",
    data: {
      nodes: [
        {
          id: "J01",
          label: "Job 1",
          state: "Awaiting review",
          progress: 100,
          tone: "warning",
        },
      ],
      edges: [],
    },
    interactive: true,
  },
  EventLog: {
    label: "Events",
    events: [
      {
        id: "e1",
        datetime: "2026-10-21T12:00:00Z",
        time: "12:00",
        message: "Committed",
        tone: "info",
      },
    ],
  },
  MetricStrip: {
    label: "Metrics",
    items: [{ id: "m1", label: "Jobs", value: 1 }],
  },
};
for (const flavor of ["svelte", "react"])
  for (const [name, base] of Object.entries(cases))
    it(`${flavor} ${name} renders meaningful native content`, async () => {
      const target = document.createElement("div");
      document.body.append(target);
      const selected = vi.fn(),
        options = { ...base, onselectedchange: selected };
      let dispose: () => Promise<void>;
      if (flavor === "svelte") {
        const app = mount(Fixture, { target, props: { name, options } });
        flushSync();
        dispose = () => unmount(app);
      } else {
        const root = createRoot(target);
        await act(() =>
          root.render(
            h(
              (adapters as Record<string, ComponentType<any>>)[name]!,
              options,
              name === "SceneViewport"
                ? h("p", null, "Static scene content")
                : undefined,
            ),
          ),
        );
        dispose = async () => {
          await act(() => root.unmount());
        };
      }
      try {
        if (name === "StateGrid" || name === "QueueView") {
          await act(() => {
            target.querySelector<HTMLButtonElement>("button")!.click();
            flushSync();
          });
          expect(selected).toHaveBeenCalledWith("J01");
        }
        if (name === "ObjectStack") {
          expect(target.textContent).toContain("100");
          expect(target.textContent).toContain("4 representatives shown");
        }
        if (name === "WorkflowDiagram") {
          expect(target.textContent).toContain("Awaiting review");
          expect(
            target.querySelector(
              '[data-mark-id="J01"] text[fill="var(--reddb-color-feedback-warning-foreground)"]',
            ),
          ).not.toBeNull();
        }
        if (name === "SceneViewport")
          expect(target.textContent).toContain("Static scene content");
        if (name === "EventLog") {
          expect(target.textContent).toContain("Committed");
          expect(
            target.querySelector<HTMLElement>("[data-event-id=e1] p")?.style
              .color,
          ).toBe("var(--reddb-color-feedback-info-foreground)");
        }
      } finally {
        await dispose();
        target.remove();
      }
    });
it("projects actual solid height and bounds representative count", () => {
  const flat = solidBox(4, 3, 1),
    tall = solidBox(4, 3, 4);
  expect(Math.min(...tall.points.map((p) => p.y))).toBeLessThan(
    Math.min(...flat.points.map((p) => p.y)),
  );
  expect(stackItems(10000, 4)).toMatchObject({ shown: 4 });
  expect(() => stackItems(-1, 4)).toThrow();
});
it("retains before filtering and rejects duplicate event identities", () => {
  const events = Array.from({ length: 20 }, (_, i) => ({
    id: String(i),
    datetime: "2026-10-21",
    time: String(i),
    message: i % 2 ? "odd" : "even",
  }));
  expect(retainedEvents(events, 4, "odd")).toMatchObject({ discarded: 16 });
  expect(retainedEvents(events, 4, "odd").events.map((e) => e.id)).toEqual([
    "17",
    "19",
  ]);
  expect(() => retainedEvents([...events, events[0]!], 4, "")).toThrow();
});
it("rejects dangling workflow ports and keeps parallel channels distinct", () => {
  const nodes = [
      { id: "a", label: "A", state: "Running" },
      { id: "b", label: "B", state: "Waiting" },
    ],
    edges = [
      { id: "e1", label: "One", source: "a", target: "b", bus: "dispatch" },
      { id: "e2", label: "Two", source: "a", target: "b", bus: "dispatch" },
    ];
  const scene = workflowLayout({ nodes, edges }, "right");
  expect(scene.edges[0]!.path).not.toBe(scene.edges[1]!.path);
  expect(() =>
    workflowLayout(
      { nodes, edges: [{ ...edges[0]!, targetPort: "missing" }] },
      "right",
    ),
  ).toThrow();
});
it("disposes a renderer that resolves after its scene was unmounted", async () => {
  const host = document.createElement("div");
  document.body.append(host);
  let resolve!: (value: any) => void;
  const factory = vi.fn(() => new Promise<any>((r) => (resolve = r))),
    status = vi.fn(),
    handle = { update: vi.fn(), pause: vi.fn(), destroy: vi.fn() };
  const controller = mountSceneEngine(host, factory, "Scene", {}, status);
  await Promise.resolve();
  controller.destroy();
  resolve(handle);
  await vi.waitFor(() => expect(handle.destroy).toHaveBeenCalledOnce());
  expect(handle.update).not.toHaveBeenCalled();
  host.remove();
});

it("keeps 24 bus channels distinct and inside the connection corridor in either direction", () => {
  const nodes = [
    { id: "a", label: "A", state: "Running" },
    { id: "b", label: "B", state: "Waiting" },
  ];
  const edges = Array.from({ length: 24 }, (_, i) => ({
    id: `e${i}`,
    label: `Channel ${i}`,
    source: "a",
    target: "b",
    bus: "dispatch",
  }));
  for (const direction of ["right", "down"] as const) {
    const scene = workflowLayout({ nodes, edges }, direction);
    expect(new Set(scene.edges.map((e) => e.path)).size).toBe(24);
    for (const edge of scene.edges) {
      const bend = Number(
        edge.path.match(direction === "right" ? / H([^ ]+)/ : / V([^ ]+)/)![1],
      );
      const start = direction === "right" ? edge.start.x : edge.start.y;
      const end = direction === "right" ? edge.end.x : edge.end.y;
      expect(bend).toBeGreaterThan(start);
      expect(bend).toBeLessThan(end);
    }
  }
});
it("keeps geometry deterministic with many independently placed volumes", () => {
  const items = Array.from({ length: 10000 }, (_, i) => ({
    id: String(i),
    label: String(i),
    x: i % 100,
    y: Math.floor(i / 100),
  }));
  const first = sceneGeometry(items),
    second = sceneGeometry([...items].reverse());
  expect(first.viewBox).toBe(second.viewBox);
  expect(first.boxes.map((b) => b.id)).toEqual(second.boxes.map((b) => b.id));
});
for (const flavor of ["svelte", "react"]) {
  it(`${flavor} updates scene data without remounting its engine and disposes on unmount`, async () => {
    const target = document.createElement("div");
    document.body.append(target);
    const handle = { update: vi.fn(), pause: vi.fn(), destroy: vi.fn() };
    const engine = vi.fn(
      (_host: HTMLElement, _context: { signal: AbortSignal }) => handle,
    );
    const options = { ...cases.SceneViewport, engine, data: { revision: 1 } };
    let update: (data: unknown) => Promise<void>, dispose: () => Promise<void>;
    if (flavor === "svelte") {
      // A reactive parent simulates real application prop updates.
      const parent = await import("./fixtures/SceneUpdates.svelte");
      const app = mount(parent.default, { target, props: { options } });
      flushSync();
      update = async () => {
        target.querySelector<HTMLButtonElement>("[data-update]")!.click();
        flushSync();
      };
      dispose = () => unmount(app);
    } else {
      const root = createRoot(target);
      await act(() => root.render(h(SceneViewport, options, "Static content")));
      update = async (data) => {
        await act(() =>
          root.render(h(SceneViewport, { ...options, data }, "Static content")),
        );
      };
      dispose = async () => {
        await act(() => root.unmount());
      };
    }
    try {
      await vi.waitFor(() => expect(engine).toHaveBeenCalledOnce());
      await update({ revision: 2 });
      await vi.waitFor(() =>
        expect(handle.update).toHaveBeenLastCalledWith({ revision: 2 }),
      );
      expect(engine).toHaveBeenCalledOnce();
    } finally {
      await dispose();
      target.remove();
    }
    expect(handle.destroy).toHaveBeenCalledOnce();
    expect(engine.mock.calls[0]![1].signal.aborted).toBe(true);
  });
  it(`${flavor} renders an empty stock count without a misleading square drawing`, async () => {
    const target = document.createElement("div");
    document.body.append(target);
    const options = { label: "Stock", count: 0 };
    let dispose: () => Promise<void>;
    if (flavor === "svelte") {
      const app = mount(Fixture, {
        target,
        props: { name: "ObjectStack", options },
      });
      flushSync();
      dispose = () => unmount(app);
    } else {
      const root = createRoot(target);
      await act(() => root.render(h(ObjectStack, options)));
      dispose = async () => {
        await act(() => root.unmount());
      };
    }
    try {
      expect(target.textContent).toContain("0");
      expect(target.querySelector("svg")).toBeNull();
    } finally {
      await dispose();
      target.remove();
    }
  });
}

it("yields follow-tail immediately to keyboard navigation and resumes only when requested", () => {
  const list = document.createElement("div");
  Object.defineProperty(list, "scrollHeight", { value: 1000 });
  Object.defineProperty(list, "clientHeight", { value: 200 });
  const pause = vi.fn(),
    controller = followEvents(list, pause);
  controller.update(true);
  expect(list.scrollTop).toBe(1000);
  list.dispatchEvent(new KeyboardEvent("keydown", { key: "Home" }));
  expect(pause).toHaveBeenCalledOnce();
  list.scrollTop = 0;
  list.dispatchEvent(new Event("scroll"));
  expect(pause).toHaveBeenCalledOnce();
  controller.update(true);
  expect(list.scrollTop).toBe(1000);
  controller.destroy();
  list.dispatchEvent(new WheelEvent("wheel"));
  expect(pause).toHaveBeenCalledOnce();
});
