import { describe, expect, it } from "vitest";
import ActivityFeed from "../src/composites/ActivityFeed.svelte";
import { activityFeed } from "../src/composites/activity-feed.variants";
import SocialContractFailures from "./fixtures/SocialContractFailures.svelte";
import SocialSurfacesConsumer from "./fixtures/SocialSurfacesConsumer.svelte";
import { expectLayoutReadiness } from "./layout-readiness";
import { classes, classesOf, render, rendered } from "./mount";

const ITEMS = [
  {
    id: "queued",
    title: "Ada queued the deployment",
    datetime: "2026-08-08T19:00:00Z",
    time: "19:00",
    description: "Production",
    href: "/deployments/queued",
  },
  {
    id: "deployed",
    title: "Lin deployed the release",
    datetime: "2026-08-08T19:05:00Z",
    time: "19:05",
  },
] as const;

describe("the deliberately failing ActivityFeed fixture", () => {
  it("demonstrates a chronological feed without ordered semantics", () => {
    const broken = rendered(render(SocialContractFailures, { failure: "unordered-feed" }));

    expect(broken.querySelectorAll("time")).toHaveLength(2);
    expect(broken.querySelector("ol")).toBeNull();
    expect(broken.querySelector("li")).toBeNull();
  });
});

describe("ActivityFeed", () => {
  it("composes the canonical Timeline as a named ordered feed", () => {
    const feed = rendered(render(ActivityFeed, { label: "Deployment activity", items: ITEMS }));

    expect(feed.tagName).toBe("OL");
    expect(feed.matches("[data-timeline][data-activity-feed]")).toBe(true);
    expect(feed.getAttribute("aria-label")).toBe("Deployment activity");
    expect(feed.querySelectorAll(":scope > li")).toHaveLength(2);
    expect([...feed.querySelectorAll("time")].map((time) => time.dateTime))
      .toEqual(ITEMS.map((item) => item.datetime));
  });

  it("preserves canonical links and native focus order", () => {
    const feed = rendered(render(ActivityFeed, { label: "Deployment activity", items: ITEMS }));
    const link = feed.querySelector<HTMLAnchorElement>('a[href="/deployments/queued"]')!;

    link.focus();
    expect(document.activeElement).toBe(link);
    expect(feed.querySelectorAll("a")).toHaveLength(1);
  });

  it("keeps Density tokenized and inherits every nested appearance axis", () => {
    const scope = rendered(render(SocialSurfacesConsumer));
    const feed = scope.querySelector<HTMLElement>("[data-activity-feed]")!;

    expect(classes(feed)).toEqual(expect.objectContaining(classesOf(activityFeed().root())));
    expect(classes(feed)).toContain("gap-[var(--reddb-spatial-gap-md)]");
    expect(scope.matches('[data-theme="application"][data-color-scheme="dark"][data-density="compact"][data-motion="reduced"]'))
      .toBe(true);
    expect(feed.hasAttribute("data-theme")).toBe(false);
    expect(feed.hasAttribute("data-color-scheme")).toBe(false);
    expect(feed.hasAttribute("data-density")).toBe(false);
    expect(feed.hasAttribute("data-motion")).toBe(false);
  });

  it("ships showcase, consumer, distributed export, and readiness evidence", () => {
    expectLayoutReadiness("activity-feed", "ActivityFeed");
  });
});
