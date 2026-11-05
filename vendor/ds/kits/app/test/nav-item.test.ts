// NavItem, as an application meets it.
//
// "Where am I" is the assertion this file is really about. An item that is
// only tinted is current to the eye and identical to everything else for
// anything that reads the page, so `aria-current` is checked in every form the
// component offers — and checked to be absent when the item is not current,
// since an item list where everything claims to be the page is no better.

import { describe, expect, it, vi } from "vitest";
import { quietControl } from "@reddb-io/design-system/base";
import NavItem from "../src/primitives/NavItem.svelte";
import { navItem } from "../src/primitives/nav-item.variants";
import { classes, classesOf, click, render, rendered, text } from "./mount";

describe("NavItem", () => {
  it("renders an <a> when it goes somewhere", () => {
    const element = rendered(render(NavItem, { label: "Tokens", href: "/tokens" }));
    expect(element.tagName).toBe("A");
    expect(element.getAttribute("href")).toBe("/tokens");
    expect(element.textContent?.trim()).toBe("Tokens");
  });

  it("renders a <button> when it only does something", () => {
    const onclick = vi.fn();
    const element = rendered(render(NavItem, { label: "Filter", onclick }));
    expect(element.tagName).toBe("BUTTON");
    expect(element.getAttribute("type")).toBe("button");

    click(element);
    expect(onclick).toHaveBeenCalledTimes(1);
  });

  it("says where you are, and says it only where you are", () => {
    const current = rendered(render(NavItem, { label: "Tokens", href: "/tokens", current: true }));
    expect(current.getAttribute("aria-current")).toBe("page");

    const inactive = rendered(render(NavItem, { label: "Themes", href: "/themes" }));
    expect(inactive.getAttribute("aria-current")).toBeNull();
  });

  it("takes the word for what it is current within", () => {
    const element = rendered(
      render(NavItem, { label: "Details", href: "/step-2", current: "step" }),
    );
    expect(element.getAttribute("aria-current")).toBe("step");
  });

  it("keeps a disabled item out of reach of pointer and keyboard alike", () => {
    const link = rendered(render(NavItem, { label: "Soon", href: "/soon", disabled: true }));
    // A disabled link is not a thing the platform has: withholding the href is
    // what makes it unfocusable, and aria-disabled is what makes it explained.
    expect(link.getAttribute("href")).toBeNull();
    expect(link.getAttribute("tabindex")).toBe("-1");
    expect(link.getAttribute("aria-disabled")).toBe("true");
    expect(classes(link).has("pointer-events-none")).toBe(true);

    const control = rendered(render(NavItem, { label: "Soon", disabled: true }));
    expect((control as HTMLButtonElement).disabled).toBe(true);
  });

  it("wears exactly the classes its variants module produces", () => {
    for (const current of [true, false]) {
      for (const disabled of [true, false]) {
        const element = rendered(render(NavItem, { label: "Tokens", current, disabled }));
        // The quiet-control contract wraps the slot (wave 6A).
        expect(classes(element)).toEqual(classesOf(quietControl({ selected: current, class: navItem({ current, disabled }).root() })));
      }
    }
  });

  it("defaults to not current and available", () => {
    expect(classes(rendered(render(NavItem, { label: "Tokens" })))).toEqual(
      classesOf(quietControl({ class: navItem({ current: false, disabled: false }).root() })),
    );
  });

  it("hides a decorative icon and keeps the trailing rail addressable", () => {
    const element = rendered(
      render(NavItem, { label: "Search", icon: text("⌕"), trailing: text("Ctrl K") }),
    );
    const icon = element.querySelector('[aria-hidden="true"]')!;
    expect(classes(icon)).toEqual(classesOf(navItem().icon()));
    expect(icon.getAttribute("aria-hidden")).toBe("true");

    const trailing = element.lastElementChild!;
    expect(classes(trailing)).toEqual(classesOf(navItem().trailing()));
    expect(trailing.getAttribute("aria-hidden")).toBeNull();
    expect(trailing.textContent?.trim()).toBe("Ctrl K");
  });

  it("lets a children snippet replace the label", () => {
    const element = rendered(render(NavItem, { label: "ignored", children: text("Custom") }));
    expect(element.textContent?.trim()).toBe("Custom");
    expect(element.textContent).not.toContain("ignored");
  });

  it("merges a caller's classes over the root slot's own", () => {
    const element = rendered(render(NavItem, { label: "Tokens", class: "w-48" }));
    expect(classes(element).has("w-48")).toBe(true);
    expect(classes(element).has("rounded-e-md")).toBe(true);
  });

  it("passes native attributes straight through", () => {
    const element = rendered(render(NavItem, { label: "Tokens", id: "nav-tokens" }));
    expect(element.id).toBe("nav-tokens");
  });

  it("marks the current item with a neutral surface and a start-edge Brand bar, never a red fill", () => {
    const current = classes(rendered(render(NavItem, { label: "Tokens", href: "/tokens", current: true })));
    const other = classes(rendered(render(NavItem, { label: "Kits", href: "/kit" })));

    expect(current.has("bg-foreground/10")).toBe(true);
    expect(current.has("font-medium")).toBe(true);
    expect(current.has("text-foreground")).toBe(true);
    expect(current.has("border-s-2")).toBe(true);
    expect(current.has("border-foreground")).toBe(true);
    expect(current.has("border-primary")).toBe(false);
    expect([...current].some((name) => /^bg-primary(?:\/|$)/.test(name))).toBe(false);

    // Every item carries the edge, transparent until current: nothing shifts.
    expect(other.has("border-s-2")).toBe(true);
    expect(other.has("border-transparent")).toBe(true);
    expect(other.has("border-foreground")).toBe(false);
  });
});
