import { readFileSync } from "node:fs";
import { join } from "node:path";
import { lintKitSource, readVocabulary } from "@reddb-io/kit-lint";
import { createRawSnippet } from "svelte";
import { describe, expect, it } from "vitest";
import ApplicationShell from "../src/composites/ApplicationShell.svelte";
import { applicationShell } from "../src/composites/application-shell.variants";
import ApplicationLayoutsConsumer from "./fixtures/ApplicationLayoutsConsumer.svelte";
import { expectLayoutReadiness } from "./layout-readiness";
import { classes, classesOf, render, rendered } from "./mount";

const FAILURE_FIXTURE = join(
  import.meta.dirname,
  "fixtures",
  "ApplicationLayoutsContractFailures.svelte",
);

describe("the deliberately failing Application shell fixture", () => {
  it("diagnoses a shell whose spatial inset is frozen against Density", () => {
    const violations = lintKitSource(
      readFileSync(FAILURE_FIXTURE, "utf8"),
      FAILURE_FIXTURE,
      readVocabulary(),
    );

    expect(violations.map(({ found }) => found)).toContain("p-4");
  });
});

describe("ApplicationShell", () => {
  it("provides the keyboard bypass and canonical focusable main landmark", () => {
    const root = render(ApplicationShell, {
      children: createRawSnippet(() => ({ render: () => "<h1>Deployments</h1>" })),
    });
    const shell = rendered(root);
    const link = shell.querySelector<HTMLAnchorElement>("[data-skip-link]")!;
    const main = shell.querySelector<HTMLElement>("main")!;

    expect(link.getAttribute("href")).toBe("#main-content");
    expect(main.id).toBe("main-content");
    expect(main.tabIndex).toBe(-1);
    expect(main.textContent).toBe("Deployments");

    link.click();
    expect(document.activeElement).toBe(main);
  });

  it("yields the main landmark to a host document when embedded", () => {
    const root = render(ApplicationShell, {
      embedded: true,
      mainId: "preview-main",
      children: createRawSnippet(() => ({ render: () => "<h1>Deployments</h1>" })),
    });
    const shell = rendered(root);
    const region = shell.querySelector<HTMLElement>('[data-application-shell-region="main"]')!;
    const link = shell.querySelector<HTMLAnchorElement>("[data-skip-link]")!;

    expect(shell.querySelector("main")).toBeNull();
    expect(region.tagName).toBe("DIV");
    expect(region.id).toBe("preview-main");
    expect(link.getAttribute("href")).toBe("#preview-main");
    link.click();
    expect(document.activeElement).toBe(region);
  });

  it("composes optional page landmarks around caller-owned content", () => {
    const shell = rendered(
      render(ApplicationShell, {
        mainId: "workspace",
        header: createRawSnippet(() => ({ render: () => "<nav>Product</nav>" })),
        children: createRawSnippet(() => ({ render: () => "<p>Workspace</p>" })),
        footer: createRawSnippet(() => ({ render: () => "<p>Status</p>" })),
      }),
    );

    expect([...shell.children].map((child) => child.tagName)).toEqual([
      "A",
      "HEADER",
      "MAIN",
      "FOOTER",
    ]);
    expect(shell.querySelector("header nav")?.textContent).toBe("Product");
    expect(shell.querySelector("main")?.id).toBe("workspace");
    expect(shell.querySelector("footer")?.textContent).toBe("Status");
    expect(shell.querySelectorAll("[data-container]")).toHaveLength(3);
    expect(shell.querySelector("main [data-stack]")).not.toBeNull();
  });

  it("keeps every appearance axis inherited and every shell inset Density-owned", () => {
    const shell = rendered(render(ApplicationShell, {}));
    const slots = applicationShell();

    expect(classes(shell)).toEqual(classesOf(slots.root()));
    const mainContainer = classes(shell.querySelector("main [data-container]")!);
    for (const className of classesOf(slots.mainContainer())) {
      expect(mainContainer.has(className)).toBe(true);
    }
    expect(mainContainer.has(
      "py-[var(--reddb-spatial-inset-lg)]",
    )).toBe(true);
    expect(shell.hasAttribute("data-theme")).toBe(false);
    expect(shell.hasAttribute("data-color-scheme")).toBe(false);
    expect(shell.hasAttribute("data-density")).toBe(false);
  });

  it("passes native attributes and merges a caller class", () => {
    const shell = rendered(
      render(ApplicationShell, { class: "relative", "aria-label": "Product application" }),
    );

    expect(shell.getAttribute("aria-label")).toBe("Product application");
    expect(classes(shell)).toEqual(classesOf(applicationShell().root({ class: "relative" })));
  });

  it("lets every region consume the viewport edge in full-width workspaces", () => {
    const shell = rendered(
      render(ApplicationShell, {
        fullWidth: true,
        header: createRawSnippet(() => ({ render: () => "<span>Header</span>" })),
        children: createRawSnippet(() => ({ render: () => "<span>Workspace</span>" })),
        footer: createRawSnippet(() => ({ render: () => "<span>Status</span>" })),
      }),
    );

    for (const container of shell.querySelectorAll("[data-container]")) {
      expect(classes(container as HTMLElement).has("max-w-none")).toBe(true);
      expect(classes(container as HTMLElement).has("px-0")).toBe(true);
    }
    const header = shell.querySelector<HTMLElement>("header")!;
    expect(classes(header).has("bg-navigation-surface")).toBe(true);
    expect(classes(header).has("shadow-elevation-sunken")).toBe(false);
    expect(classes(header.querySelector<HTMLElement>("[data-container]")!).has("py-0")).toBe(true);
  });

  it("compiles as a public consumer inside nested appearance scopes", () => {
    const root = render(ApplicationLayoutsConsumer);
    const shell = root.querySelector<HTMLElement>("[data-application-shell]")!;

    expect(shell.closest('[data-theme="base"][data-color-scheme="dark"][data-density="compact"]'))
      .not.toBeNull();
    expect(shell.hasAttribute("data-density")).toBe(false);
  });

  it("ships complete showcase, consumer, distributed export, and readiness evidence", () => {
    expectLayoutReadiness("application-shell", "ApplicationShell");
  });
});
