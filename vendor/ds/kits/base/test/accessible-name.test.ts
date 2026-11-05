// The accessible-name contract (wave 5A), proven through the public Base subpath.
//
// Input, Textarea and Select are named by a Field, `aria-label`,
// `aria-labelledby` or a native <label>; an icon-only Button by `aria-label`.
// The props stay optional in the type (requiring them is a later, breaking
// decision), so a control that reaches the page unnamed warns once in
// development through `warnA11y`, the sibling of `warnDeprecated`.

import { afterEach, beforeEach, describe, expect, it, vi, type MockInstance } from "vitest";
import { warnA11y } from "@reddb-io/design-system/base";
import AccessibleNameConsumer from "./fixtures/AccessibleNameConsumer.svelte";
import { render } from "./mount";

let warn: MockInstance<typeof console.warn>;
beforeEach(() => {
  warn = vi.spyOn(console, "warn").mockImplementation(() => {});
});
afterEach(() => {
  warn.mockRestore();
});

const warnings = () => warn.mock.calls.map(([message]) => String(message));
type Case = Parameters<typeof AccessibleNameConsumer>[1]["case"];

describe("the accessible-name contract", () => {
  it.each<Case>(["field", "aria-label", "aria-labelledby", "label-for", "wrapping-label"])(
    "stays silent for a field named through %s",
    (which) => {
      render(AccessibleNameConsumer, { case: which });
      expect(warnings()).toEqual([]);
    },
  );

  it.each<[Case, string]>([
    ["unnamed-input", "Input"],
    ["unnamed-textarea", "Textarea"],
    ["unnamed-select", "Select"],
  ])("warns once in development when %s has no name", (which, component) => {
    render(AccessibleNameConsumer, { case: which });
    render(AccessibleNameConsumer, { case: which });
    const mine = warnings().filter((message) => message.startsWith(`[reddb ${component}]`));
    // Two unnamed mounts, one warning: once per session.
    expect(mine).toEqual([expect.stringMatching(/no accessible name/)]);
  });

  it("warns when a Field's label is empty", () => {
    render(AccessibleNameConsumer, { case: "empty-field" });
    expect(warnings()).toEqual([expect.stringMatching(/^\[reddb Field\] has an empty `label`/)]);
  });

  it.each<Case>(["text-button", "sr-only-button", "labelled-icon-button"])("stays silent for a named %s", (which) => {
    render(AccessibleNameConsumer, { case: which });
    expect(warnings()).toEqual([]);
  });

  it("warns when an icon-only Button has no aria-label", () => {
    render(AccessibleNameConsumer, { case: "icon-button" });
    expect(warnings()).toEqual([expect.stringMatching(/^\[reddb Button\] .*no accessible name.*aria-label/)]);
  });

  it("is exported beside warnDeprecated and dedupes per component and problem", () => {
    warnA11y("Probe", "is unnamed", "Name it.");
    warnA11y("Probe", "is unnamed", "Name it.");
    expect(warnings()).toEqual(["[reddb Probe] is unnamed. Name it."]);
  });
});
