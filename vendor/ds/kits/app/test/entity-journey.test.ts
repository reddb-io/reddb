import { mount, unmount, flushSync } from "svelte";
import { act, createElement as h } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import { EntityJourney } from "../react/src/EntityJourney";
import { journeyStages, type JourneyStage } from "../src/core/entity-journey";
import Fixture from "./fixtures/EntityJourney.svelte";
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
const items: JourneyStage[] = [
  { id: "a", label: "Accepted", state: "complete", elapsed: "4 ms" },
  {
    id: "b",
    label: "Database",
    state: "blocked",
    description: "Waiting for lock",
  },
  { id: "c", label: "Respond", state: "upcoming" },
];
it("keeps observed state independent of order and duration; rejects ambiguous ids and states", () => {
  expect(journeyStages(items).map((item) => item.state)).toEqual([
    "complete",
    "blocked",
    "upcoming",
  ]);
  expect(() => journeyStages([items[0]!, items[0]!])).toThrow();
  expect(() =>
    journeyStages([{ ...items[0]!, state: "unknown" as any }]),
  ).toThrow();
  expect(
    journeyStages([{ ...items[0]!, stateLabel: "Concluído" }])[0]!.stateLabel,
  ).toBe("Concluído");
});
for (const flavor of ["react", "svelte"])
  it(`${flavor} inspects without advancing, preserves failure/skips and supports read-only and empty trails`, async () => {
    const target = document.createElement("div");
    document.body.append(target);
    const changed = vi.fn();
    let options: any = {
      label: "Execution",
      items,
      selected: "a",
      onselectedchange: changed,
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
      await act(() => root.render(h(EntityJourney, options)));
      update = async () => {
        await act(() => root.render(h(EntityJourney, options)));
      };
      dispose = async () => {
        await act(() => root.unmount());
      };
    }
    try {
      const blocked = target.querySelector<HTMLButtonElement>(
        "[data-journey-stage=b] button",
      )!;
      await act(() => {
        blocked.click();
        flushSync();
      });
      expect(changed).toHaveBeenLastCalledWith("b");
      expect(blocked.getAttribute("aria-pressed")).toBe("false");
      expect(target.querySelector("[aria-current=step]")).toBeNull();
      options = {
        ...options,
        selected: "b",
        items: [
          { ...items[0], state: "skipped" },
          { ...items[1], state: "failed" },
          { ...items[2], state: "current" },
        ],
      };
      await update();
      expect(target.textContent).toContain("Failed");
      expect(target.textContent).toContain("Skipped");
      expect(
        target
          .querySelector("[aria-current=step]")
          ?.getAttribute("data-journey-stage"),
      ).toBe("c");
      options = { ...options, onselectedchange: undefined };
      await update();
      expect(target.querySelector("button")).toBeNull();
      options = { ...options, items: [] };
      await update();
      expect(target.textContent).toContain("No stages observed");
    } finally {
      await dispose();
      target.remove();
    }
  });
