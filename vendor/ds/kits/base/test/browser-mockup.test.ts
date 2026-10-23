import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import {
  BrowserMockup,
  DEFAULT_BROWSER_MOCKUP_RATIO,
  browserMockup as browserMockupAppearance,
} from "../src/index";
import MockupContractFailures from "./fixtures/MockupContractFailures.svelte";
import { classes, classesOf, render, rendered, styleOf } from "./mount";

const deploymentPreview = () =>
  createRawSnippet(() => ({
    render: () =>
      '<article aria-label="Deployment preview"><button type="button">Inspect deployment</button></article>',
  }));

describe("the deliberately failing mockup fixtures", () => {
  it("exposes interactive browser chrome that must be decorative", () => {
    const root = render(MockupContractFailures, { failure: "interactive-chrome" });
    const chrome = root.querySelector<HTMLElement>("[data-broken-browser-chrome]")!;

    expect(chrome.getAttribute("aria-hidden")).not.toBe("true");
    expect(chrome.querySelector("a, input, button")).not.toBeNull();
  });
});

describe("the Base BrowserMockup", () => {
  it("keeps caller semantics and focus while hiding its fake browser controls", () => {
    const element = rendered(
      render(BrowserMockup, {
        address: "https://example.test/deployments",
        class: "max-w-4xl",
        children: deploymentPreview(),
      }),
    );
    const chrome = element.querySelector<HTMLElement>("[data-browser-mockup-chrome]")!;
    const viewport = element.querySelector<HTMLElement>("[data-browser-mockup-viewport]")!;
    const article = viewport.querySelector<HTMLElement>('article[aria-label="Deployment preview"]')!;
    const button = article.querySelector<HTMLButtonElement>("button")!;

    expect(element.getAttribute("role")).toBe("presentation");
    expect(element.hasAttribute("aria-hidden")).toBe(false);
    expect(chrome.getAttribute("aria-hidden")).toBe("true");
    expect(chrome.querySelector("a, button, input, select, textarea")).toBeNull();
    expect(chrome.textContent).toContain("https://example.test/deployments");
    expect(article.getAttribute("aria-label")).toBe("Deployment preview");
    button.focus();
    expect(document.activeElement).toBe(button);
    expect(styleOf(viewport, "aspect-ratio")).toBe(`${DEFAULT_BROWSER_MOCKUP_RATIO} / 1`);
    expect(classes(element)).toEqual(
      classesOf(browserMockupAppearance().root({ class: "max-w-4xl" })),
    );
    expect(element.hasAttribute("data-theme")).toBe(false);
    expect(element.hasAttribute("data-color-scheme")).toBe(false);
    expect(element.hasAttribute("data-density")).toBe(false);
  });
});
