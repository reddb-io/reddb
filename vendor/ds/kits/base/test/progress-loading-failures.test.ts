import { describe, expect, it } from "vitest";
import ProgressLoadingContractFailures from "./fixtures/ProgressLoadingContractFailures.svelte";
import { render } from "./mount";

describe("the deliberately failing progress and loading fixtures", () => {
  it("demonstrates a determinate progress bar without an accessible value", () => {
    const root = render(ProgressLoadingContractFailures, {
      failure: "missing-progress-value",
    });
    const progress = root.querySelector<HTMLElement>("[data-broken-progress]")!;

    expect(
      root.querySelector<HTMLElement>("[data-broken-progress-fill]")!.style.width,
    ).toBe("72%");
    expect(progress.getAttribute("role")).toBe("progressbar");
    expect(progress.hasAttribute("aria-valuenow")).toBe(false);
  });

  it("demonstrates a spinner that never announces that its region is busy", () => {
    const root = render(ProgressLoadingContractFailures, { failure: "silent-spinner" });
    const loading = root.querySelector<HTMLElement>("[data-broken-loading]")!;

    expect(loading.querySelector("[data-broken-spinner]")).not.toBeNull();
    expect(loading.getAttribute("role")).not.toBe("status");
    expect(loading.hasAttribute("aria-busy")).toBe(false);
  });
});
