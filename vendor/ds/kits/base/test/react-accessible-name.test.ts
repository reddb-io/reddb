import { act, createElement as h, createRef } from "react";
import { createRoot } from "react-dom/client";
import { afterEach, describe, expect, it, vi } from "vitest";
import { Button } from "../react/src/Button";
import { Input } from "../react/src/Input";
import { Select } from "../react/src/Select";
import { Textarea } from "../react/src/Textarea";
import { Field } from "../react/src/Field";

Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
afterEach(() => { document.body.replaceChildren(); vi.restoreAllMocks(); });
describe("native React accessibility diagnostics", () => {
  it("warns about unnamed controls and action labels overridden by row context, without changing DOM", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const container = document.createElement("div"); document.body.append(container);
    const inputRef = createRef<HTMLInputElement>();
    const root = createRoot(container);
    await act(() => root.render(h("div", null,
      h(Input, { ref: inputRef }), h(Textarea), h(Select), h(Button, null, h("svg", { "aria-hidden": true })),
      h("span", { id: "context" }, "HTTPS access"), h(Button, { "aria-labelledby": "context" }, "Configure HTTPS"),
    )));
    expect(warn.mock.calls.map(call => String(call[0])).join("\n")).toContain("[reddb Input]");
    expect(warn.mock.calls.map(call => String(call[0])).join("\n")).toContain("[reddb Textarea]");
    expect(warn.mock.calls.map(call => String(call[0])).join("\n")).toContain("[reddb Select]");
    expect(warn.mock.calls.map(call => String(call[0])).join("\n")).toContain("visible text is missing");
    expect(inputRef.current).toBe(container.querySelector("input"));
    expect(container.querySelector("button[aria-labelledby]")?.getAttribute("aria-labelledby")).toBe("context");
    await act(() => root.unmount());
    expect(inputRef.current).toBeNull();
  });

  it("accepts native labels and Field associations without inventing names", async () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const container = document.createElement("div"); document.body.append(container);
    const root = createRoot(container);
    await act(() => root.render(h("div", null,
      h(Field, { label: "Name", help: "Used for billing", children: control => h(Input, control) }),
      h("label", null, "Notes", h(Textarea)),
      h(Select, { "aria-label": "Region" }), h(Button, null, "Save"),
    )));
    expect(warn).not.toHaveBeenCalled();
    await act(() => root.unmount());
  });
});
