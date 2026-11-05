import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import { Card } from "@reddb-io/design-system/base";
import { render, rendered } from "./mount";

describe("the Base Card busy contract", () => {
  it("announces background work without removing the current content", () => {
    const card = rendered(render(Card, {
      title: "Query results",
      busy: true,
      busyLabel: "Running query against local.reddb",
      children: createRawSnippet(() => ({ render: () => "<p>Four previous rows</p>" })),
    }));

    expect(card.getAttribute("aria-busy")).toBe("true");
    expect(card.querySelector("[data-card-busy]")?.textContent).toContain("Running query against local.reddb");
    expect(card.textContent).toContain("Four previous rows");
  });

  it("omits the busy announcement when idle", () => {
    const card = rendered(render(Card, { title: "Query results" }));
    expect(card.hasAttribute("aria-busy")).toBe(false);
    expect(card.querySelector("[data-card-busy]")).toBeNull();
  });
});
