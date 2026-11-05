import { existsSync, readFileSync } from "node:fs";
import { join } from "node:path";
import { flushSync, tick } from "svelte";
import { describe, expect, it, vi } from "vitest";
import {
  CommandPalette,
  commandPalette,
  type CommandPaletteCommand,
} from "../src/index";
import { popover } from "@reddb-io/design-system/base";
import CommandPaletteContractFailures from "./fixtures/CommandPaletteContractFailures.svelte";
import CommandPaletteConsumer from "./fixtures/CommandPaletteConsumer.svelte";
import { classes, classesOf, render } from "./mount";

const COMMANDS: readonly CommandPaletteCommand[] = [
  { id: "settings", label: "Open settings" },
  { id: "deploy", label: "Deploy application" },
];
const REPO_ROOT = join(import.meta.dirname, "..", "..", "..");

async function settle(): Promise<void> {
  await tick();
  await new Promise((resolve) => setTimeout(resolve, 10));
  flushSync();
}

async function focusRestorationFailures(root: HTMLElement): Promise<string[]> {
  const trigger = root.querySelector<HTMLButtonElement>("[data-broken-palette-trigger]")!;
  trigger.focus();
  trigger.click();
  await settle();
  root.querySelector<HTMLInputElement>('input[aria-label="Command"]')!.focus();
  root.querySelector<HTMLButtonElement>("[data-broken-command]")!.click();
  await settle();
  return document.activeElement === trigger ? [] : ["focus was not restored to the palette invoker"];
}

describe("the deliberately failing CommandPalette fixture", () => {
  it("diagnoses a palette that closes without restoring its invoking focus", async () => {
    const root = render(CommandPaletteContractFailures, { failure: "focus-not-restored" });

    expect(await focusRestorationFailures(root)).toEqual([
      "focus was not restored to the palette invoker",
    ]);
  });
});

describe("the application CommandPalette", () => {
  it("focuses canonical command search and restores its invoker after selection", async () => {
    const onselect = vi.fn();
    const root = render(CommandPalette, {
      triggerLabel: "Search commands",
      contentLabel: "Commands",
      label: "Command",
      commands: COMMANDS,
      onselect,
    });
    const trigger = root.querySelector<HTMLButtonElement>("[data-popover-trigger]")!;
    trigger.focus();
    trigger.click();
    await settle();

    const input = document.querySelector<HTMLInputElement>('[role="combobox"]')!;
    expect(document.activeElement).toBe(input);

    input.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
    await settle();
    input.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    await settle();

    expect(onselect).toHaveBeenCalledWith(COMMANDS[0]);
    expect(document.querySelector("[data-popover-surface]")).toBeNull();
    expect(document.activeElement).toBe(trigger);
  });

  it("runs the same command again on a second invocation", async () => {
    const onselect = vi.fn();
    const root = render(CommandPalette, {
      triggerLabel: "Search commands",
      contentLabel: "Commands",
      label: "Command",
      commands: COMMANDS,
      onselect,
    });
    const trigger = root.querySelector<HTMLButtonElement>("[data-popover-trigger]")!;

    for (let run = 0; run < 2; run += 1) {
      trigger.click();
      await settle();
      const input = document.querySelector<HTMLInputElement>('[role="combobox"]')!;
      input.dispatchEvent(new KeyboardEvent("keydown", { key: "ArrowDown", bubbles: true }));
      await settle();
      input.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
      await settle();
    }

    expect(onselect).toHaveBeenCalledTimes(2);
    expect(onselect).toHaveBeenNthCalledWith(2, COMMANDS[0]);
  });

  it("keeps every appearance axis inherited through token-backed extension seams", async () => {
    const root = render(CommandPalette, {
      triggerLabel: "Search commands",
      contentLabel: "Commands",
      label: "Command",
      commands: COMMANDS,
      open: true,
      class: "max-w-xl",
      contentClass: "shadow-none",
      inputClass: "font-mono",
    });
    await settle();
    const palette = root.querySelector<HTMLElement>("[data-command-palette]")!;
    const surface = document.querySelector<HTMLElement>("[data-popover-surface]")!;
    const input = surface.querySelector<HTMLInputElement>('[role="combobox"]')!;
    const slots = commandPalette();

    expect(classes(palette)).toEqual(classesOf(slots.root({ class: "max-w-xl" })));
    expect(classes(surface)).toEqual(
      classesOf(popover({ class: slots.content({ class: "shadow-none" }) })),
    );
    expect(classes(input).has("font-mono")).toBe(true);
    expect(classes(surface).has("p-[var(--reddb-spatial-inset-md)]")).toBe(true);
    expect(palette.hasAttribute("data-theme")).toBe(false);
    expect(palette.hasAttribute("data-color-scheme")).toBe(false);
    expect(palette.hasAttribute("data-density")).toBe(false);
  });

  it("compiles through the distributed app subpath inside a nested appearance scope", () => {
    const root = render(CommandPaletteConsumer);
    const palette = root.querySelector<HTMLElement>("[data-command-palette]")!;

    expect(
      palette.closest(
        '[data-theme="base"][data-color-scheme="dark"][data-density="compact"][data-motion="reduced"]',
      ),
    ).not.toBeNull();
    expect(palette.hasAttribute("data-theme")).toBe(false);
    expect(palette.hasAttribute("data-color-scheme")).toBe(false);
    expect(palette.hasAttribute("data-density")).toBe(false);
    expect(palette.hasAttribute("data-motion")).toBe(false);
  });

  it("ships complete showcase, accessibility, appearance, and readiness evidence", () => {
    const readiness = JSON.parse(
      readFileSync(join(REPO_ROOT, "scripts", "producer", "readiness.json"), "utf8"),
    ) as Array<{
      id: string;
      baselineDisposition: string;
      canonicalKit: string;
      export: string;
      evidence: Record<string, string[]>;
    }>;
    const definition = readiness.find(({ id }) => id === "command-palette")!;

    expect(definition).toMatchObject({
      baselineDisposition: "implemented",
      canonicalKit: "application",
      export: "CommandPalette",
    });
    for (const paths of Object.values(definition.evidence)) {
      expect(paths.length).toBeGreaterThan(0);
      for (const path of paths) expect(existsSync(join(REPO_ROOT, path)), path).toBe(true);
    }

    const snapshot = JSON.parse(
      readFileSync(
        join(REPO_ROOT, "packages", "baseline", "snapshot", "baseline-v1.json"),
        "utf8",
      ),
    ) as { capabilities: Array<{ id: string; classification: string; status: string }> };
    expect(snapshot.capabilities.find(({ id }) => id === "command-palette")).toMatchObject({
      classification: "application",
      status: "implemented",
    });
  });
});
