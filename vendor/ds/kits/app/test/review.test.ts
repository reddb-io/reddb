import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import Review from "../src/composites/Review.svelte";
import { review } from "../src/composites/review.variants";
import SocialContractFailures from "./fixtures/SocialContractFailures.svelte";
import SocialSurfacesConsumer from "./fixtures/SocialSurfacesConsumer.svelte";
import { expectLayoutReadiness } from "./layout-readiness";
import { classes, classesOf, render, rendered } from "./mount";

const body = createRawSnippet(() => ({ render: () => "<p>Clear and dependable.</p>" }));
const actions = createRawSnippet(() => ({
  render: () => '<button type="button">Mark helpful</button>',
}));

describe("the deliberately failing Review fixture", () => {
  it("demonstrates a rating conveyed by colour alone", () => {
    const broken = rendered(render(SocialContractFailures, { failure: "colour-only-rating" }));
    const rating = broken.querySelector<HTMLElement>('[aria-label="Rating"]')!;

    expect(rating.textContent).toBe("★★★★★");
    expect(rating.querySelector("input, meter, output")).toBeNull();
    expect(rating.textContent).not.toMatch(/\d/);
  });
});

describe("Review", () => {
  it("composes canonical Card, MediaObject, Avatar, and Rating contracts", () => {
    const reviewElement = rendered(render(Review, {
      author: "Ada Lovelace",
      avatarFallback: "AL",
      rating: 4,
      maxRating: 5,
      datetime: "2026-08-08",
      time: "8 August 2026",
      children: body,
      actions,
    }));

    expect(reviewElement.tagName).toBe("ARTICLE");
    expect(reviewElement.getAttribute("aria-label")).toBe("Review by Ada Lovelace");
    expect(reviewElement.querySelector("[data-card]")).not.toBeNull();
    expect(reviewElement.querySelector("[data-media-object]")).not.toBeNull();
    expect(reviewElement.querySelector("[data-avatar]")?.getAttribute("aria-label")).toBe("Ada Lovelace");
    expect(reviewElement.querySelector("time")?.dateTime).toBe("2026-08-08");
    expect(reviewElement.querySelectorAll('[data-rating] input[type="radio"]')).toHaveLength(5);
    expect(reviewElement.querySelector("[data-rating] output")?.textContent).toContain("4 of 5");
  });

  it("conveys the score through symbols, text, and native state rather than colour alone", () => {
    const reviewElement = rendered(render(Review, {
      author: "Ada Lovelace",
      avatarFallback: "AL",
      rating: 4,
      maxRating: 5,
      children: body,
    }));
    const marks = [...reviewElement.querySelectorAll<HTMLElement>("[data-rating-mark]")];
    const checked = reviewElement.querySelector<HTMLInputElement>('[data-rating] input:checked')!;

    expect(marks.map((mark) => mark.textContent)).toEqual(["★", "★", "★", "★", "☆"]);
    expect(checked.value).toBe("4");
    expect(reviewElement.querySelector("[data-rating] output")?.textContent).toContain("4 of 5");
  });

  it("preserves caller-owned action focus and inherits every nested appearance axis", () => {
    const scope = rendered(render(SocialSurfacesConsumer));
    const reviewElement = scope.querySelector<HTMLElement>("[data-review]")!;
    const action = reviewElement.querySelector<HTMLButtonElement>("button")!;

    expect(classes(reviewElement)).toEqual(expect.objectContaining(classesOf(review().root())));
    expect(classes(reviewElement.querySelector("[data-review-content]")!))
      .toContain("gap-[var(--reddb-spatial-gap-md)]");
    action.focus();
    expect(document.activeElement).toBe(action);
    expect(reviewElement.hasAttribute("data-theme")).toBe(false);
    expect(reviewElement.hasAttribute("data-color-scheme")).toBe(false);
    expect(reviewElement.hasAttribute("data-density")).toBe(false);
    expect(reviewElement.hasAttribute("data-motion")).toBe(false);
  });

  it("ships showcase, consumer, distributed export, and readiness evidence", () => {
    expectLayoutReadiness("review", "Review");
  });
});
