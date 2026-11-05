import { mount, unmount, flushSync } from "svelte";
import { act, createElement as h } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import { WorkspaceTabs } from "../react/src/WorkspaceTabs";
import { ChangeReview } from "../react/src/ChangeReview";
import WorkspaceFixture from "./fixtures/WorkspaceTabs.svelte";
import ReviewFixture from "./fixtures/ChangeReview.svelte";
import { workspaceTabs } from "../src/core/workspace-tabs";
import { reviewChanges } from "../src/core/change-review";
import { shortcutGroups } from "../src/core/shortcut-help";
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
it("refuses ambiguous identities and invalid review/chord data", () => {
  expect(() =>
    workspaceTabs([
      { id: "a", label: "A" },
      { id: "a", label: "B" },
    ]),
  ).toThrow();
  expect(() =>
    reviewChanges([
      { id: "a", label: "A", before: "1", after: "2", state: "guessed" as any },
    ]),
  ).toThrow();
  expect(() =>
    shortcutGroups([
      { id: "a", label: "Global", items: [{ id: "x", label: "X", keys: [] }] },
    ]),
  ).toThrow();
});
async function harness(
  flavor: string,
  Component: any,
  Fixture: any,
  initial: any,
) {
  const target = document.createElement("div");
  document.body.append(target);
  if (flavor === "svelte") {
    const app = mount(Fixture, { target, props: { options: initial } });
    flushSync();
    return {
      target,
      update: async (options: any) => {
        flushSync(() => app.update(options));
      },
      dispose: async () => {
        await unmount(app);
        target.remove();
      },
    };
  }
  const root = createRoot(target);
  await act(() => root.render(h(Component, initial)));
  return {
    target,
    update: async (options: any) => {
      await act(() => root.render(h(Component, options)));
    },
    dispose: async () => {
      await act(() => root.unmount());
      target.remove();
    },
  };
}
for (const flavor of ["svelte", "react"]) {
  it(`${flavor} requests closing a dirty tab without discarding it and recovers focus after accepted removal`, async () => {
    const close = vi.fn(),
      select = vi.fn();
    let options: any = {
      label: "Editors",
      items: [
        { id: "a", label: "Draft", dirty: true },
        { id: "b", label: "Result", busy: true },
        { id: "c", label: "Unavailable", disabled: true },
      ],
      selected: "a",
      oncloserequest: close,
      onselectedchange: select,
    };
    const app = await harness(flavor, WorkspaceTabs, WorkspaceFixture, options);
    try {
      expect(
        app.target
          .querySelector('[role="tab"][aria-selected="true"]')
          ?.getAttribute("aria-label"),
      ).toBe("Draft");
      const button = app.target.querySelector<HTMLButtonElement>(
        '[aria-label="Close Draft"]',
      )!;
      await act(() => {
        button.focus();
        button.click();
        flushSync();
      });
      expect(close).toHaveBeenCalledWith("a");
      expect(app.target.querySelectorAll('[role="tab"]')).toHaveLength(3);
      options = { ...options, items: options.items.slice(1), selected: "b" };
      await app.update(options);
      expect(document.activeElement?.getAttribute("data-workspace-tab")).toBe(
        "b",
      );
      expect(
        app.target.querySelector<HTMLButtonElement>('[data-workspace-tab="c"]')
          ?.disabled,
      ).toBe(true);
      const current = app.target.querySelector<HTMLElement>(
        '[data-workspace-tab="b"]',
      )!;
      await act(() => {
        current.dispatchEvent(
          new KeyboardEvent("keydown", { key: "Delete", bubbles: true }),
        );
        flushSync();
      });
      expect(close).toHaveBeenLastCalledWith("b");
    } finally {
      await app.dispose();
    }
  });
  it(`${flavor} preserves exact changes and requests only pending/failed ids after partial results`, async () => {
    const apply = vi.fn(),
      discard = vi.fn();
    let options: any = {
      title: "Review",
      items: [
        {
          id: "a",
          label: "Applied",
          before: "0",
          after: "1",
          state: "applied",
        },
        {
          id: "b",
          label: "Conflict",
          before: "2",
          after: "3",
          state: "failed",
          error: "Version conflict",
        },
      ],
      onapplyrequest: apply,
      ondiscardrequest: discard,
    };
    const app = await harness(flavor, ChangeReview, ReviewFixture, options);
    try {
      const button = [
        ...app.target.querySelectorAll<HTMLButtonElement>("button"),
      ].find((b) => b.textContent?.trim() === "Apply changes")!;
      await act(() => {
        button.click();
        flushSync();
      });
      expect(apply).toHaveBeenCalledWith(["b"]);
      expect(
        app.target.querySelector('[data-review-change="b"] [role="alert"]')
          ?.textContent,
      ).toBe("Version conflict");
      expect(options.items[1].state).toBe("failed");
      options = { ...options, blockedReason: "Read only" };
      await app.update(options);
      expect(
        [...app.target.querySelectorAll<HTMLButtonElement>("button")].find(
          (b) => b.textContent?.trim() === "Apply changes",
        )?.disabled,
      ).toBe(true);
      const discardButton = app.target.querySelector<HTMLButtonElement>(
        '[aria-label="Discard Conflict"]',
      )!;
      await act(() => {
        discardButton.click();
        flushSync();
      });
      expect(discard).toHaveBeenCalledWith(["b"]);
      options = { ...options, applying: true };
      await app.update(options);
      expect(
        [...app.target.querySelectorAll<HTMLButtonElement>("button")].every(
          (button) => button.disabled,
        ),
      ).toBe(true);
    } finally {
      await app.dispose();
    }
  });
}
