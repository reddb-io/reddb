import { mount, unmount, flushSync } from "svelte";
import { act, createElement as h } from "react";
import { createRoot } from "react-dom/client";
import { expect, it, vi } from "vitest";
import { SecretValue } from "../react/src/SecretValue";
import Fixture from "./fixtures/SecretValue.svelte";
import { createSecretExposure } from "../src/core/secret-value";
Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
it("expires without extending exposure on refresh, blocks stale approvals and masks replacement values", () => {
  vi.useFakeTimers();
  const changed = vi.fn(),
    publish = vi.fn();
  let options: any = {
    label: "Token",
    value: "sample-alpha",
    revealed: true,
    revealable: true,
    hideAfter: 1000,
    onrevealedchange: changed,
  };
  const controller = createSecretExposure(() => options, publish);
  try {
    controller.sync();
    vi.advanceTimersByTime(600);
    controller.sync();
    vi.advanceTimersByTime(400);
    expect(publish).toHaveBeenLastCalledWith(false);
    expect(changed).toHaveBeenLastCalledWith(false, "timeout");
    controller.sync();
    expect(publish).toHaveBeenLastCalledWith(false);
    options = { ...options, revealed: false };
    controller.sync();
    options = { ...options, revealed: true };
    controller.sync();
    expect(publish).toHaveBeenLastCalledWith(true);
    options = { ...options, value: "sample-beta" };
    expect(controller.matches(options.value)).toBe(false);
    controller.sync();
    expect(publish).toHaveBeenLastCalledWith(false);
    expect(changed).toHaveBeenLastCalledWith(false, "value-change");
  } finally {
    controller.destroy();
    vi.useRealTimers();
  }
});
for (const flavor of ["react", "svelte"])
  it(`${flavor} keeps masked plaintext out of the DOM and immediately masks revoked access`, async () => {
    const target = document.createElement("div");
    document.body.append(target);
    const changed = vi.fn();
    let options: any = {
      label: "Token",
      value: "sample-sensitive-value",
      revealable: true,
      onrevealedchange: changed,
    };
    let update: (next: any) => Promise<void>, dispose: () => Promise<void>;
    if (flavor === "svelte") {
      const app = mount(Fixture, { target, props: { options } });
      flushSync();
      update = async (next) => {
        flushSync(() => app.update(next));
      };
      dispose = () => unmount(app);
    } else {
      const root = createRoot(target);
      await act(() => root.render(h(SecretValue, options)));
      update = async (next) => {
        await act(() => root.render(h(SecretValue, next)));
      };
      dispose = async () => {
        await act(() => root.unmount());
      };
    }
    try {
      expect(target.innerHTML).not.toContain("sample-sensitive-value");
      await act(() => {
        target.querySelector<HTMLButtonElement>("button")!.click();
        flushSync();
      });
      expect(changed).toHaveBeenLastCalledWith(true, "control");
      expect(target.innerHTML).not.toContain("sample-sensitive-value");
      options = { ...options, revealed: true };
      await update(options);
      expect(target.textContent).toContain("sample-sensitive-value");
      options = { ...options, revealable: false };
      await update(options);
      expect(target.innerHTML).not.toContain("sample-sensitive-value");
      expect(target.querySelector("button")).toBeNull();
    } finally {
      await dispose();
      target.remove();
    }
  });
