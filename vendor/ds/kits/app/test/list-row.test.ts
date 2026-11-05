// ListRow, as an application meets it.
//
// The element the row renders is the contract. A row that does something has
// to be reachable by keyboard, which means it must really be an <a> or a
// <button> and not a <div> wearing a click handler — and a row that does
// nothing must not wear the styling that says it does. Both directions are
// asserted here, because only asserting the first would let a decorative row
// look pressable forever.

import { describe, expect, it, vi } from "vitest";
import { quietControl } from "@reddb-io/design-system/base";
import ListRow from "../src/primitives/ListRow.svelte";
import { LIST_ROW_SIZES, listRow } from "../src/primitives/list-row.variants";
import { classes, classesOf, click, render, rendered, text } from "./mount";

describe("ListRow", () => {
  it("renders a plain <div> when the row does nothing", () => {
    const element = rendered(render(ListRow, { title: "reddb-01" }));
    expect(element.tagName).toBe("DIV");
    expect(element.textContent?.trim()).toBe("reddb-01");
  });

  it("renders an <a> when given a destination", () => {
    const element = rendered(render(ListRow, { title: "reddb-01", href: "/nodes/reddb-01" }));
    expect(element.tagName).toBe("A");
    expect(element.getAttribute("href")).toBe("/nodes/reddb-01");
  });

  it("renders a <button> when given something to do, and calls it once", () => {
    const onclick = vi.fn();
    const element = rendered(render(ListRow, { title: "reddb-01", onclick }));
    expect(element.tagName).toBe("BUTTON");
    // A row inside a form must not submit it just by being pressed.
    expect(element.getAttribute("type")).toBe("button");

    click(element);
    expect(onclick).toHaveBeenCalledTimes(1);
  });

  it("wears the interactive styling only when it is really interactive", () => {
    const inert = rendered(render(ListRow, { title: "reddb-01" }));
    expect(classes(inert)).toEqual(classesOf(listRow({ interactive: false }).root()));
    expect(classes(inert).has("cursor-pointer")).toBe(false);

    for (const props of [{ href: "/nodes" }, { onclick: () => {} }]) {
      const live = rendered(render(ListRow, { title: "reddb-01", ...props }));
      // An interactive row wears the quiet-control contract around its slot (wave 6A).
      expect(classes(live)).toEqual(classesOf(quietControl({ ink: "inherit", class: listRow({ interactive: true }).root() })));
    }
  });

  it("wears exactly the classes its variants module produces", () => {
    for (const size of LIST_ROW_SIZES) {
      for (const selected of [true, false]) {
        const element = rendered(render(ListRow, { title: "reddb-01", size, selected }));
        expect(classes(element)).toEqual(
          classesOf(listRow({ size, selected, interactive: false }).root()),
        );
      }
    }
  });

  it("defaults to the medium size, unselected", () => {
    expect(classes(rendered(render(ListRow, { title: "reddb-01" })))).toEqual(
      classesOf(listRow({ size: "md", selected: false, interactive: false }).root()),
    );
  });

  it("says which row the list is about, where the platform has a word for it", () => {
    const link = rendered(render(ListRow, { title: "reddb-01", href: "/nodes", selected: true }));
    expect(link.getAttribute("aria-current")).toBe("true");
    // On a <div> there is nothing to be current within, so the attribute would
    // be noise rather than information.
    const plain = rendered(render(ListRow, { title: "reddb-01", selected: true }));
    expect(plain.getAttribute("aria-current")).toBeNull();
  });

  it("renders the title and description as two lines", () => {
    const element = rendered(
      render(ListRow, { title: "reddb-01", description: "eu-west-1 · leader" }),
    );
    const lines = [...element.querySelectorAll("span")].filter(
      (span) => span.children.length === 0,
    );
    expect(lines.map((line) => line.textContent)).toEqual(["reddb-01", "eu-west-1 · leader"]);
  });

  it("lets a children snippet replace the title and description", () => {
    const element = rendered(
      render(ListRow, { title: "reddb-01", description: "ignored", children: text("Custom") }),
    );
    expect(element.textContent?.trim()).toBe("Custom");
    expect(element.textContent).not.toContain("ignored");
  });

  it("renders the leading and trailing rails only when given one", () => {
    const both = rendered(
      render(ListRow, { title: "reddb-01", leading: text("●"), trailing: text("3") }),
    );
    expect(classes(both.firstElementChild!)).toEqual(classesOf(listRow({}).leading()));
    expect(classes(both.lastElementChild!)).toEqual(classesOf(listRow({}).trailing()));

    const neither = rendered(render(ListRow, { title: "reddb-01" }));
    expect(neither.children.length).toBe(1);
  });

  it("merges a caller's classes over the root slot's own", () => {
    const element = rendered(render(ListRow, { title: "reddb-01", class: "rounded-md" }));
    expect(classes(element).has("rounded-md")).toBe(true);
    expect(classes(element).has("flex")).toBe(true);
  });

  it("passes native attributes straight through", () => {
    const element = rendered(render(ListRow, { title: "reddb-01", id: "row-1" }));
    expect(element.id).toBe("row-1");
  });
});
