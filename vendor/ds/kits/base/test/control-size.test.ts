// Fields line up with the actions beside them (step 5 of the 2026-10 contrast
// and spacing audit): Input, Select and Combobox take Button's three control
// heights through `controlSize`, while the native `size` attribute still
// passes through untouched; a NavigationMenu's inset follows its size.

import { describe, expect, it } from "vitest";
import {
  Combobox,
  Input,
  Select,
  button,
  navigationMenu,
} from "@reddb-io/design-system/base";
import { classes, classesOf, render, rendered } from "./mount";

const SIZES = ["sm", "md", "lg"] as const;
const INSET = { sm: "inset-sm", md: "inset-md", lg: "inset-lg" } as const;
const role = (name: string) => `[var(--reddb-spatial-${name})]`;

describe("field control sizes", () => {
  for (const size of SIZES) {
    it(`gives Input, Select and Combobox the ${size} Button's height and inset`, () => {
      const buttonClasses = classesOf(button({ size }));
      const height = `h-${role(`control-height-${size}`)}`;
      const inset = `px-${role(INSET[size])}`;
      expect(buttonClasses.has(`min-${height}`) && buttonClasses.has(inset)).toBe(true);

      const field = rendered(render(Input, { controlSize: size }));
      const choice = rendered(render(Select, { controlSize: size, options: [{ value: "a", label: "A" }] }));
      for (const element of [field, choice]) {
        expect(classes(element).has(height), `${element.tagName} ${size}`).toBe(true);
        expect(classes(element).has(inset), `${element.tagName} ${size}`).toBe(true);
      }

      const combo = render(Combobox, { label: "Region", controlSize: size, options: [{ value: "a", label: "A" }] });
      const comboInput = combo.querySelector<HTMLInputElement>('[role="combobox"]')!;
      expect(classes(comboInput).has(height), `Combobox ${size}`).toBe(true);
      // The end clearance is one control height, so a trigger fits flush.
      expect(classes(comboInput).has(`pe-${role(`control-height-${size}`)}`), `Combobox ${size}`).toBe(true);
    });
  }

  it("defaults to md, and still forwards the native size attribute", () => {
    const field = rendered(render(Input, { size: 12 })) as HTMLInputElement;
    expect(field.size).toBe(12);
    expect(classes(field).has(`h-${role("control-height-md")}`)).toBe(true);

    const choice = rendered(render(Select, { size: 3, options: [{ value: "a", label: "A" }] })) as HTMLSelectElement;
    expect(choice.size).toBe(3);
    expect(classes(choice).has(`h-${role("control-height-md")}`)).toBe(true);
  });
});

describe("navigation control insets", () => {
  it("insets a NavigationMenu control by its size", () => {
    for (const size of SIZES) {
      expect(classesOf(navigationMenu({ size }).control()).has(`px-${role(INSET[size])}`), size).toBe(true);
    }
  });
});
