import { describe, expect, it } from "vitest";
import NumericChronologyContractFailures from "./fixtures/NumericChronologyContractFailures.svelte";
import NumericChronologyConsumer from "./fixtures/NumericChronologyConsumer.svelte";
import {
  Timeline,
  timeline as timelineAppearance,
  type TimelineItem,
} from "./fixtures/numeric-chronology-consumer";
import { classes, classesOf, render, rendered } from "./mount";

describe("the deliberately failing Timeline fixture", () => {
  it("demonstrates chronology without ordered semantics", () => {
    const element = rendered(render(NumericChronologyContractFailures, {
      failure: "unordered-timeline",
    }));

    expect(element.querySelectorAll("time")).toHaveLength(2);
    expect(element.querySelector("ol")).toBeNull();
    expect(element.querySelector("li")).toBeNull();
  });
});

describe("the Base Timeline", () => {
  const items: readonly TimelineItem[] = [
    {
      id: "queued",
      title: "Queued",
      datetime: "2026-08-08T19:00:00Z",
      time: "19:00",
      description: "Waiting for capacity",
      href: "/deployments/queued",
    },
    {
      id: "deployed",
      title: "Deployed",
      datetime: "2026-08-08T19:05:00Z",
      time: "19:05",
    },
  ];

  it("owns ordered chronology and machine-readable times", () => {
    const element = rendered(render(Timeline, { label: "Deployment history", items }));

    expect(element.tagName).toBe("OL");
    expect(element.getAttribute("aria-label")).toBe("Deployment history");
    expect(element.querySelectorAll(":scope > li")).toHaveLength(2);
    expect([...element.querySelectorAll("time")].map((time) => time.dateTime))
      .toEqual(items.map((item) => item.datetime));
    expect(element.textContent).toContain("Waiting for capacity");
  });

  it("composes the canonical Link without changing native focus order", () => {
    const root = render(Timeline, { label: "Deployment history", items });
    const link = root.querySelector<HTMLAnchorElement>('a[href="/deployments/queued"]')!;

    link.focus();
    expect(document.activeElement).toBe(link);
    expect(link.textContent).toContain("Queued");
    expect(root.querySelectorAll("a")).toHaveLength(1);
  });

  it("inherits nested appearance and Density", () => {
    const scope = render(NumericChronologyConsumer)
      .querySelector<HTMLElement>("[data-numeric-chronology-scope]")!;
    const element = scope.querySelector<HTMLElement>("[data-timeline]")!;

    expect(classes(element)).toEqual(classesOf(timelineAppearance().root()));
    expect(classes(element).has("gap-[var(--reddb-spatial-gap-md)]")).toBe(true);
    expect(scope.getAttribute("data-density")).toBe("compact");
    expect(element.hasAttribute("data-theme")).toBe(false);
    expect(element.hasAttribute("data-color-scheme")).toBe(false);
    expect(element.hasAttribute("data-contrast")).toBe(false);
    expect(element.hasAttribute("data-density")).toBe(false);
  });
});
