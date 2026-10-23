import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import {
  DEFAULT_PHONE_MOCKUP_RATIO,
  PhoneMockup,
  phoneMockup as phoneMockupAppearance,
} from "../src/index";
import MockupContractFailures from "./fixtures/MockupContractFailures.svelte";
import { classes, classesOf, render, rendered, styleOf } from "./mount";

const queuePreview = () =>
  createRawSnippet(() => ({
    render: () =>
      '<nav aria-label="Phone queue"><button type="button">Retry job</button></nav>',
  }));

describe("the deliberately failing frame fixture", () => {
  it("swallows the accessible content when the whole frame is hidden", () => {
    const root = render(MockupContractFailures, { failure: "swallowed-content" });
    const article = root.querySelector<HTMLElement>('article[aria-label="Deployment preview"]')!;

    expect(article.closest('[aria-hidden="true"]')).not.toBeNull();
  });
});

describe("the Base PhoneMockup", () => {
  it("keeps caller landmarks and focus outside its decorative phone body", () => {
    const element = rendered(
      render(PhoneMockup, { class: "max-w-xs", children: queuePreview() }),
    );
    const chrome = element.querySelector<HTMLElement>("[data-phone-mockup-chrome]")!;
    const viewport = element.querySelector<HTMLElement>("[data-phone-mockup-viewport]")!;
    const navigation = viewport.querySelector<HTMLElement>('nav[aria-label="Phone queue"]')!;
    const button = navigation.querySelector<HTMLButtonElement>("button")!;

    expect(element.getAttribute("role")).toBe("presentation");
    expect(element.hasAttribute("aria-hidden")).toBe(false);
    expect(chrome.getAttribute("aria-hidden")).toBe("true");
    expect(chrome.querySelector("a, button, input, select, textarea")).toBeNull();
    expect(navigation.getAttribute("aria-label")).toBe("Phone queue");
    button.focus();
    expect(document.activeElement).toBe(button);
    expect(styleOf(viewport, "aspect-ratio")).toBe(`${DEFAULT_PHONE_MOCKUP_RATIO} / 1`);
    expect(classes(element)).toEqual(
      classesOf(phoneMockupAppearance().root({ class: "max-w-xs" })),
    );
    expect(element.hasAttribute("data-theme")).toBe(false);
    expect(element.hasAttribute("data-color-scheme")).toBe(false);
    expect(element.hasAttribute("data-density")).toBe(false);
  });
});
