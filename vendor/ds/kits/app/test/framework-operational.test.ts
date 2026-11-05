import { mount, unmount, flushSync } from "svelte";
import { act, createElement as h, type ComponentType } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import * as ReactKit from "../react/src";
import Fixture from "./fixtures/Operational.svelte";
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
const cases = {
  TaskCard: {
    item: {
      id: "job",
      title: "Compile",
      state: "blocked",
      progress: 100,
      reason: "Waiting for approval",
    },
  },
  TelemetryRow: {
    label: "Worker",
    value: 32,
    max: 96,
    stateLabel: "Running",
    condition: "Locked",
  },
  PropertyPanel: {
    title: "Machine",
    items: [{ label: "Capacity", value: 96 }],
  },
  PhaseSummary: {
    label: "Release",
    gates: [
      { id: "execute", label: "Execute", state: "passed" },
      { id: "approve", label: "Approve", state: "pending" },
    ],
  },
  PlaybackControls: {
    label: "Replay",
    playing: false,
    position: 2,
    duration: 10,
  },
};
for (const flavor of ["svelte", "react"])
  for (const [name, base] of Object.entries(cases))
    it(`${flavor} ${name} uses independent state and native controls`, async () => {
      const target = document.createElement("div");
      document.body.append(target);
      const changed = vi.fn(),
        step = vi.fn(),
        options = { ...base, onplayingchange: changed, onstep: step };
      let dispose: () => Promise<void>;
      if (flavor === "svelte") {
        const app = mount(Fixture, { target, props: { name, options } });
        flushSync();
        dispose = () => unmount(app);
      } else {
        const root = createRoot(target);
        await act(() =>
          root.render(
            h((ReactKit as Record<string, ComponentType<any>>)[name]!, options),
          ),
        );
        dispose = async () => {
          await act(() => root.unmount());
        };
      }
      try {
        if (name === "TaskCard") {
          expect(target.textContent).toContain("Blocked");
          expect(target.textContent).toContain("Waiting for approval");
        }
        if (name === "TelemetryRow") {
          expect(target.textContent).toContain("Running");
          expect(target.textContent).toContain("Locked");
        }
        if (name === "PhaseSummary") {
          expect(
            target
              .querySelector("[data-gate-id=approve]")
              ?.getAttribute("data-state"),
          ).toBe("pending");
        }
        if (name === "PropertyPanel")
          expect(target.querySelector("dt")?.textContent).toContain("Capacity");
        if (name === "PlaybackControls") {
          await act(() => {
            (target.querySelector("button") as HTMLButtonElement).click();
            flushSync();
          });
          expect(changed).toHaveBeenCalledWith(true);
          expect(step).not.toHaveBeenCalled();
        }
      } finally {
        await dispose();
        target.remove();
      }
    });
