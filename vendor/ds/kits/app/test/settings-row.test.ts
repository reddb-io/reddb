import { mount, unmount, flushSync } from "svelte";
import { createElement as h } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it } from "vitest";
import { SettingsRow } from "../react/src/SettingsRow";
import { Button } from "../../base/react/src/Button";
import { Input } from "../../base/react/src/Input";
import Fixture from "./fixtures/SettingsRows.svelte";

afterEach(() => document.body.replaceChildren());
describe("settings-row associations in both native adapters", () => {
  it.each(["React", "Svelte"])("%s keeps field help separate and preserves native action names", async flavor => {
    let mounted: ReturnType<typeof mount> | undefined;
    if (flavor === "React") {
      document.body.innerHTML = renderToStaticMarkup(h("div", null,
        h(SettingsRow, { label: "Signing secret", description: "At least 16 characters.", id: "secret", children: control => h(Input, { ...control, type: "password" }) }),
        h(SettingsRow, { label: "HTTPS and mobile access", description: "Connect another device.", actions: h(Button, null, "Configure HTTPS") }),
      ));
    } else {
      mounted = mount(Fixture, { target: document.body });
      flushSync();
    }
    try {
      const input = document.querySelector("input")!;
      expect(input.labels?.[0]?.textContent).toBe("Signing secret");
      expect(input.id).toBe("secret");
      expect(document.getElementById(input.getAttribute("aria-describedby")!)?.textContent).toBe("At least 16 characters.");
      const action = document.querySelector("button")!;
      expect(action.textContent?.trim()).toBe("Configure HTTPS");
      expect(action.hasAttribute("aria-labelledby")).toBe(false);
      expect(action.hasAttribute("aria-label")).toBe(false);
    } finally { if (mounted) await unmount(mounted); }
  });
});
