import { readFileSync } from "node:fs";
import { basename, join } from "node:path";
import { describe, expect, it } from "vitest";
import { BASE_COMPONENTS, BASE_COMPOSITES, BASE_PRIMITIVES } from "../src/index";
import { KIT_MANIFEST, SRC_DIR, kitComponentFiles } from "../tools/paths";

// What the Base Kit is, checked rather than described.
//
// Two claims matter here, and both are mechanical: the Kit routes to every
// consumer and inherits nothing (it is the root every other Kit declares,
// ADR 0004), and its public surface lists exactly the components on disk. The
// second is worth having before there is a component: it is the seam the Logo
// (issue #46) arrives through, and it fails the moment a component is added
// without being exported.

describe("the Base Kit's routing manifest", () => {
  const manifest = JSON.parse(readFileSync(KIT_MANIFEST, "utf8")) as {
    name: string;
    audience: "*" | string[];
    parents?: string[];
  };

  it("is named base", () => {
    expect(manifest.name).toBe("base");
  });

  it("routes to every consumer", () => {
    // Not an allowlist: no consumer can be outside the Kit every Kit inherits.
    expect(manifest.audience).toBe("*");
  });

  it("inherits nothing — it is the root of the Kit graph", () => {
    expect(manifest.parents ?? []).toEqual([]);
  });
});

describe("the Base Kit's public surface", () => {
  it("lists exactly the components on disk", () => {
    const onDisk = kitComponentFiles().map((file) => basename(file, ".svelte")).sort();

    expect([...BASE_COMPONENTS]).toEqual(onDisk);
  });

  it("exports one module per listed component", () => {
    const index = readFileSync(join(SRC_DIR, "index.ts"), "utf8");

    for (const component of BASE_COMPONENTS) {
      expect(index).toContain(`export { default as ${component} }`);
    }
  });

  it("classifies every canonical composition and partitions the public surface", () => {
    expect(BASE_COMPOSITES).toEqual([
      "Accordion",
      "AlertDialog",
      "AppearanceSwitch",
      "Avatar",
      "Breadcrumbs",
      "BrowserMockup",
      "BulletsSlide",
      "Calendar",
      "Card",
      "Carousel",
      "Checkbox",
      "CodeBlock",
      "Combobox",
      "DateField",
      "DatePicker",
      "DateRangeField",
      "DateRangePicker",
      "Drawer",
      "DropdownMenu",
      "Disclosure",
      "Field",
      "FigureSlide",
      "FileInput",
      "GridList",
      "InlineEdit",
      "ListContainer",
      "Navbar",
      "NavigationMenu",
      "Notification",
      "NumberField",
      "OneTimeCodeInput",
      "Pagination",
      "PhoneInput",
      "PhoneMockup",
      "Popover",
      "QuoteSlide",
      "RadioGroup",
      "RangeCalendar",
      "Rating",
      "SearchSelect",
      "SecretValue",
      "SectionSlide",
      "Slider",
      "Statistic",
      "Steps",
      "Swap",
      "Switch",
      "Tabs",
      "TimeField",
      "TimeRangeField",
      "Timeline",
      "TitleSlide",
      "ToggleButton",
      "ToggleGroup",
      "TwoColumnSlide",
      "WindowMockup",
    ]);
    expect(new Set([...BASE_PRIMITIVES, ...BASE_COMPOSITES])).toEqual(new Set(BASE_COMPONENTS));
    expect(BASE_PRIMITIVES.filter((name) => BASE_COMPOSITES.includes(name as never))).toEqual([]);
  });
});
