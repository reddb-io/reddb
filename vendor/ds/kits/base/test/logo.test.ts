// The Logo, as an application meets it.
//
// Four of ADR 0004's decisions are behaviour rather than prose, and each one is
// asked here in the form it would fail in: the surface selection matrix, the
// refusal that names a gap instead of papering over it, the clearspace the box
// carries whether or not anyone remembers it, and the minimum below which a
// Logo is an error rather than a smaller Logo. The `href` form is the fifth —
// the accessible home link a navbar wants.

import { describe, expect, it } from "vitest";
import Logo from "../src/Logo.svelte";
import {
  BRAND_RELEASE,
  LOGO_LAYOUTS,
  LOGO_SURFACES,
  MARKS,
  MarkNotShippedError,
  selectMark,
  shipsMark,
  type LogoLayout,
  type LogoSurface,
} from "../src/logo.marks";
import {
  CLEARSPACE_RATIO,
  DEFAULT_SYMBOL_PX,
  LogoTooSmallError,
  MINIMUM_SYMBOL_PX,
  logoBox,
} from "../src/logo.box";
import { activeSurface, surfaceOfColorScheme } from "../src/logo.surface";
import { logo, logoMark } from "../src/logo.variants";
import { classes, classesOf, render, rendered, styleOf, withColorScheme } from "./mount";

/** The <img> inside the Logo's box — the Mark itself. */
function markImage(element: HTMLElement): HTMLImageElement {
  const image = element.querySelector("img");
  expect(image, "the Logo rendered no Mark").not.toBeNull();
  return image as HTMLImageElement;
}

/** Every (layout, on) pair, whether or not the release ships a Mark for it. */
const MATRIX: readonly { layout: LogoLayout; on: LogoSurface }[] = LOGO_LAYOUTS.flatMap((layout) =>
  LOGO_SURFACES.map((on) => ({ layout, on })),
);

describe("the selection matrix", () => {
  it("covers the product of both axes, not a list of combinations", () => {
    // Without this, a silently empty axis would make every case below vacuous.
    expect(LOGO_LAYOUTS.length).toBeGreaterThan(0);
    expect(LOGO_SURFACES.length).toBeGreaterThan(0);
    expect(MATRIX.length).toBe(LOGO_LAYOUTS.length * LOGO_SURFACES.length);
  });

  it("renders the Mark the pinned release ships for each shipped pair", () => {
    for (const { layout, on } of MATRIX) {
      if (!shipsMark(layout, on)) continue;
      const element = rendered(render(Logo, { layout, on }));
      const mark = selectMark(layout, on);

      expect(element.getAttribute("data-logo-mark")).toBe(mark.file);
      expect(markImage(element).getAttribute("src")).toBe(mark.src);
      // …and says which cell of the matrix it is, which is what makes the
      // showcase's own matrix readable rather than five similar pictures.
      expect(element.getAttribute("data-logo-layout")).toBe(layout);
      expect(element.getAttribute("data-logo-on")).toBe(on);
    }
  });

  it("takes the Brand's variant for each local surface", () => {
    // The naming is the Brand's, and the mapping is the whole of what `on`
    // means: inverse/white are for a dark BACKGROUND, not for a dark Theme.
    expect(MARKS.horizontal.light?.file).toBe("reddb-horizontal-color.svg");
    expect(MARKS.horizontal.dark?.file).toBe("reddb-horizontal-inverse.svg");
    expect(MARKS.stacked.light?.file).toBe("reddb-stacked-color.svg");
    expect(MARKS.stacked.dark?.file).toBe("reddb-stacked-inverse.svg");
    expect(MARKS.icon.light?.file).toBe("reddb-icon-color.svg");
    expect(MARKS.symbol.light?.file).toBe("reddb-symbol-black.svg");
    expect(MARKS.symbol.dark?.file).toBe("reddb-symbol-white.svg");
    expect(MARKS.wordmark.light?.file).toBe("reddb-wordmark-black.svg");
    expect(MARKS.wordmark.dark?.file).toBe("reddb-wordmark-white.svg");
  });

  it("defaults the surface from the active Color Scheme", () => {
    // ADR 0004's surface default, read off the same root
    // attribute the cascade reads. No `on` is given anywhere here.
    withColorScheme("dark", () => {
      const element = rendered(render(Logo, { layout: "horizontal" }));
      expect(element.getAttribute("data-logo-on")).toBe("dark");
      expect(element.getAttribute("data-logo-mark")).toBe("reddb-horizontal-inverse.svg");
    });

    withColorScheme("light", () => {
      const element = rendered(render(Logo, { layout: "horizontal" }));
      expect(element.getAttribute("data-logo-on")).toBe("light");
      expect(element.getAttribute("data-logo-mark")).toBe("reddb-horizontal-color.svg");
    });
  });

  it("follows a Color Scheme change after mounting", async () => {
    document.documentElement.setAttribute("data-color-scheme", "light");
    const element = rendered(render(Logo, { layout: "horizontal" }));
    const lightSource = element.querySelector("img")?.getAttribute("src");
    expect(element.getAttribute("data-logo-on")).toBe("light");

    document.documentElement.setAttribute("data-color-scheme", "dark");
    await new Promise<void>((resolve) => setTimeout(resolve, 0));

    expect(element.getAttribute("data-logo-on")).toBe("dark");
    expect(element.getAttribute("data-logo-mark")).toBe("reddb-horizontal-inverse.svg");
    expect(element.querySelector("img")?.getAttribute("src")).toBe(MARKS.horizontal.dark?.src);
    expect(element.querySelector("img")?.getAttribute("src")).not.toBe(lightSource);
    document.documentElement.removeAttribute("data-color-scheme");
  });

  it("reads an absent or unknown Color Scheme as the light surface", () => {
    for (const scheme of [null, "gloaming-not-yet-shipped"]) {
      withColorScheme(scheme, () => {
        expect(activeSurface(document)).toBe("light");
        expect(rendered(render(Logo, {})).getAttribute("data-logo-on")).toBe("light");
      });
    }
  });

  it("lets the caller override the Color Scheme, because the surface is local", () => {
    // The dark hero on a light marketing page (ADR 0004) — the case the prop
    // exists for, and the one a Theme could never have answered.
    withColorScheme("light", () => {
      const element = rendered(render(Logo, { layout: "stacked", on: "dark" }));
      expect(element.getAttribute("data-logo-on")).toBe("dark");
      expect(element.getAttribute("data-logo-mark")).toBe("reddb-stacked-inverse.svg");
    });
  });

  it("answers with no document at all, which is what a server render is", () => {
    expect(activeSurface(null)).toBe("light");
    expect(activeSurface(undefined)).toBe("light");
    expect(surfaceOfColorScheme("dark")).toBe("dark");
    expect(surfaceOfColorScheme("light")).toBe("light");
  });

  it("defaults to the horizontal layout", () => {
    expect(rendered(render(Logo, {})).getAttribute("data-logo-layout")).toBe("horizontal");
  });
});

describe("the refusal, when the release ships no such Mark", () => {
  // ADR 0004: "A missing variant refuses, never falls back and is never
  // invented locally (today: `icon` has no inverse)."

  it("is exactly one gap today, and it is the icon on dark", () => {
    const missing = MATRIX.filter(({ layout, on }) => !shipsMark(layout, on));
    expect(missing).toEqual([{ layout: "icon", on: "dark" }]);
  });

  it("throws rather than rendering, when the component is asked for it", () => {
    expect(() => render(Logo, { layout: "icon", on: "dark" })).toThrow(MarkNotShippedError);
  });

  it("throws under the dark Color Scheme too, where the default chooses it", () => {
    withColorScheme("dark", () => {
      expect(() => render(Logo, { layout: "icon" })).toThrow(MarkNotShippedError);
    });
  });

  it("names the gap and the pinned release it is missing from", () => {
    const error = (() => {
      try {
        selectMark("icon", "dark");
        return null;
      } catch (err) {
        return err as MarkNotShippedError;
      }
    })();

    expect(error).toBeInstanceOf(MarkNotShippedError);
    expect(error!.layout).toBe("icon");
    expect(error!.on).toBe("dark");
    expect(error!.release).toBe(BRAND_RELEASE);
    // The message is the first draft of the Brand proposal, so it has to carry
    // both facts a reader needs: what is missing, and from which release.
    expect(error!.message).toContain("icon");
    expect(error!.message).toContain("dark");
    expect(error!.message).toContain(BRAND_RELEASE);
    expect(error!.message).toContain("ADR 0004");
    // …and what to do meanwhile, which is the only part that is advice.
    expect(error!.message).toContain('layout="horizontal"');
    expect(error!.message).toContain('layout="stacked"');
  });

  it("never falls back to the other surface's drawing", () => {
    // The failure this is guarding against is silent, so it is worth asking
    // directly: no code path returns the light Mark for a dark surface.
    expect(MARKS.icon.dark).toBeUndefined();
    try {
      selectMark("icon", "dark");
      expect.unreachable("selectMark returned a Mark for a variant that does not exist");
    } catch (err) {
      expect(err).toBeInstanceOf(MarkNotShippedError);
    }
  });
});

describe("clearspace, which the box carries intrinsically", () => {
  // ADR 0004: clearspace is 25% of symbol height, it is inside the component's
  // own box, and there is no opt-out prop.

  it("is a quarter of the symbol's height, on every side", () => {
    for (const layout of LOGO_LAYOUTS) {
      const box = logoBox(selectMark(layout, "light"), 48);
      expect(box.clearspace).toBe(48 * CLEARSPACE_RATIO);
      expect(box.clearspace).toBe(12);
    }
  });

  it("makes the box the Mark plus clearspace on all four sides", () => {
    const mark = selectMark("horizontal", "light");
    const box = logoBox(mark, 32);

    expect(box.symbol).toBe(32);
    expect(box.markHeight).toBe(32);
    expect(box.markWidth).toBeCloseTo(127.26784, 5);
    expect(box.clearspace).toBe(8);
    expect(box.width).toBeCloseTo(box.markWidth + 16, 5);
    expect(box.height).toBe(48);
  });

  it("keeps one size meaning one symbol across layouts of different shapes", () => {
    // The stacked Mark is taller than the horizontal one at the same `size`,
    // because its symbol is a smaller share of the drawing — which is exactly
    // what makes `size` comparable between them.
    const horizontal = logoBox(selectMark("horizontal", "light"), 32);
    const stacked = logoBox(selectMark("stacked", "light"), 32);
    const icon = logoBox(selectMark("icon", "light"), 32);

    expect(horizontal.symbol).toBe(stacked.symbol);
    expect(stacked.symbol).toBe(icon.symbol);
    expect(horizontal.clearspace).toBe(stacked.clearspace);
    expect(stacked.markHeight).toBeCloseTo(49.41536, 5);
    expect(stacked.markWidth).toBeCloseTo(44.8, 5);
    expect(icon.markWidth).toBe(32);
    expect(icon.markHeight).toBe(32);
  });

  it("renders the clearspace as the box's own padding", () => {
    const element = rendered(render(Logo, { layout: "horizontal", on: "light", size: 32 }));
    expect(styleOf(element, "padding")).toBe("8px");

    const image = markImage(element);
    expect(styleOf(image, "width")).toBe("127.268px");
    expect(styleOf(image, "height")).toBe("32px");
  });

  it("has no opt-out, at any size a caller may ask for", () => {
    // The one property that has to hold for every size rather than for a
    // chosen one: there is no prop, and no size, that yields a bare Mark.
    for (const size of [MINIMUM_SYMBOL_PX, 24, DEFAULT_SYMBOL_PX, 64, 200]) {
      const element = rendered(render(Logo, { layout: "icon", on: "light", size }));
      expect(styleOf(element, "padding")).toBe(`${size * CLEARSPACE_RATIO}px`);
    }
  });

  it("defaults to a size a caller did not have to choose", () => {
    const element = rendered(render(Logo, { layout: "icon", on: "light" }));
    expect(styleOf(element, "padding")).toBe(`${DEFAULT_SYMBOL_PX * CLEARSPACE_RATIO}px`);
    expect(styleOf(markImage(element), "height")).toBe(`${DEFAULT_SYMBOL_PX}px`);
  });
});

describe("the minimum size, which is an error and not a smaller Logo", () => {
  it("accepts the minimum itself", () => {
    expect(() => render(Logo, { layout: "icon", on: "light", size: MINIMUM_SYMBOL_PX })).not.toThrow();
  });

  it("refuses anything under it, from the component and from the arithmetic", () => {
    expect(() => logoBox(selectMark("icon", "light"), MINIMUM_SYMBOL_PX - 1)).toThrow(
      LogoTooSmallError,
    );
    expect(() => render(Logo, { layout: "horizontal", on: "light", size: 8 })).toThrow(
      LogoTooSmallError,
    );
  });

  it("refuses a size that is not a length at all", () => {
    for (const size of [0, -32, Number.NaN, Number.POSITIVE_INFINITY]) {
      expect(() => logoBox(selectMark("icon", "light"), size)).toThrow(LogoTooSmallError);
    }
  });

  it("says what was asked for, what the floor is, and why there is no opt-out", () => {
    try {
      logoBox(selectMark("horizontal", "light"), 10);
      expect.unreachable("logoBox accepted a below-minimum size");
    } catch (err) {
      const error = err as LogoTooSmallError;
      expect(error).toBeInstanceOf(LogoTooSmallError);
      expect(error.size).toBe(10);
      expect(error.minimum).toBe(MINIMUM_SYMBOL_PX);
      expect(error.message).toContain("10");
      expect(error.message).toContain(String(MINIMUM_SYMBOL_PX));
      expect(error.message).toContain("ADR 0004");
    }
  });

  it("is one floor for every layout, because the symbol is one drawing", () => {
    for (const layout of LOGO_LAYOUTS) {
      expect(() => logoBox(selectMark(layout, "light"), MINIMUM_SYMBOL_PX)).not.toThrow();
      expect(() => logoBox(selectMark(layout, "light"), MINIMUM_SYMBOL_PX - 0.5)).toThrow(
        LogoTooSmallError,
      );
    }
  });
});

describe("the pure Mark, without an href", () => {
  it("renders a <span> carrying the Mark as content", () => {
    const element = rendered(render(Logo, { layout: "horizontal", on: "light" }));
    expect(element.tagName).toBe("SPAN");
    expect(element.hasAttribute("href")).toBe(false);
    expect(element.hasAttribute("aria-label")).toBe(false);
    // On its own the Mark IS the content, so it carries the Brand's name.
    expect(markImage(element).getAttribute("alt")).toBe("RedDB");
    expect(markImage(element).hasAttribute("aria-hidden")).toBe(false);
  });

  it("wears exactly the classes its variants module produces", () => {
    const element = rendered(render(Logo, {}));
    expect(classes(element)).toEqual(classesOf(logo({ interactive: false })));
    expect(classes(markImage(element))).toEqual(classesOf(logoMark()));
  });

  it("merges a caller's classes over its own", () => {
    const element = rendered(render(Logo, { class: "align-middle" }));
    expect(classes(element).has("align-middle")).toBe(true);
    expect(classes(element).has("inline-flex")).toBe(true);
  });

  it("passes native attributes straight through", () => {
    const element = rendered(render(Logo, { id: "footer-logo", title: "reddb.io" }));
    expect(element.id).toBe("footer-logo");
    expect(element.getAttribute("title")).toBe("reddb.io");
  });

  it("carries no script and no handler, so a static render is zero JS", () => {
    const element = rendered(render(Logo, {}));
    expect(element.querySelector("script")).toBeNull();
    for (const attribute of element.getAttributeNames()) {
      expect(attribute.startsWith("on")).toBe(false);
    }
  });
});

describe("the href form: the accessible home link", () => {
  it("renders a real <a> pointing where it was told", () => {
    const element = rendered(render(Logo, { href: "/" }));
    expect(element.tagName).toBe("A");
    expect(element.getAttribute("href")).toBe("/");
  });

  it("says where it goes, for anyone who cannot see the drawing", () => {
    const element = rendered(render(Logo, { href: "/" }));
    expect(element.getAttribute("aria-label")).toBe("reddb.io home");
    // …and the Mark stops being content, so the destination is announced once.
    expect(markImage(element).getAttribute("alt")).toBe("");
    expect(markImage(element).getAttribute("aria-hidden")).toBe("true");
  });

  it("lets the caller say it differently", () => {
    const element = rendered(render(Logo, { href: "/home", label: "reddb.io — início" }));
    expect(element.getAttribute("aria-label")).toBe("reddb.io — início");
  });

  it("draws its focus ring on the box, which puts it outside the clearspace", () => {
    // The ring is on the element that already INCLUDES the clearspace padding,
    // so "outside the clearspace" is construction rather than an offset
    // someone has to keep in step with the clearspace ratio.
    const element = rendered(render(Logo, { href: "/", size: 32 }));
    expect(classes(element)).toEqual(classesOf(logo({ interactive: true })));
    // Focus is ink (ADR 0023): a 2px outline at a 2px offset.
    expect(classes(element).has("focus-visible:outline-2")).toBe(true);
    expect(classes(element).has("focus-visible:outline-focus")).toBe(true);
    expect(styleOf(element, "padding")).toBe("8px");
  });

  it("is still the same Logo — same selection, same physics", () => {
    withColorScheme("dark", () => {
      const element = rendered(render(Logo, { href: "/", layout: "stacked" }));
      expect(element.getAttribute("data-logo-mark")).toBe("reddb-stacked-inverse.svg");
    });
    expect(() => render(Logo, { href: "/", layout: "icon", on: "dark" })).toThrow(
      MarkNotShippedError,
    );
    expect(() => render(Logo, { href: "/", size: 4 })).toThrow(LogoTooSmallError);
  });

  it("passes anchor attributes straight through", () => {
    const element = rendered(render(Logo, { href: "https://reddb.io", target: "_blank", rel: "noreferrer" }));
    expect(element.getAttribute("target")).toBe("_blank");
    expect(element.getAttribute("rel")).toBe("noreferrer");
  });

  it("carries no handler either", () => {
    const element = rendered(render(Logo, { href: "/" }));
    for (const attribute of element.getAttributeNames()) {
      expect(attribute.startsWith("on")).toBe(false);
    }
  });
});

describe("how the browser is told to fetch the Mark", () => {
  // The Logo is the only component in either Kit that renders an image, so it
  // is the only place these hints can live. They are asked for here in the
  // form they would fail in: silently, on the wrong element.

  it("decodes off the parser's thread, in both modes and without being asked", () => {
    // Fixed rather than a prop: nothing about a Logo needs the decode to land
    // in the same frame as the markup around it, which is all `sync` buys.
    for (const props of [{}, { href: "/" }]) {
      expect(markImage(rendered(render(Logo, props))).getAttribute("decoding")).toBe("async");
    }
  });

  it("fetches eagerly by default, because a masthead is where a Logo lives", () => {
    for (const props of [{}, { href: "/" }]) {
      expect(markImage(rendered(render(Logo, props))).getAttribute("loading")).toBe("eager");
    }
  });

  it("lets a footer Logo defer, which is the caller's call and not the Kit's", () => {
    const element = rendered(render(Logo, { loading: "lazy" }));
    expect(markImage(element).getAttribute("loading")).toBe("lazy");
  });

  it("leaves priority to the browser until someone has a reason to say", () => {
    // An absent fetchpriority is not the same as `auto`: it is the Kit having
    // no opinion, which is the honest default for a component that cannot see
    // the page it is on.
    for (const props of [{}, { href: "/" }]) {
      expect(markImage(rendered(render(Logo, props))).hasAttribute("fetchpriority")).toBe(false);
    }
    const element = rendered(render(Logo, { href: "/", fetchpriority: "high" }));
    expect(markImage(element).getAttribute("fetchpriority")).toBe("high");
  });

  it("puts every one of them on the <img>, never on the box", () => {
    // The regression this whole block exists for. `rest` is spread on the box,
    // so before these were named props a caller's `loading="lazy"` landed on a
    // <span> or an <a> — attributes those elements have no use for, dropped
    // without a word. A hint that silently does nothing is worse than one that
    // is missing, because the caller believes it took.
    for (const props of [{}, { href: "/" }]) {
      const element = rendered(
        render(Logo, { ...props, loading: "lazy", fetchpriority: "low" }),
      );
      for (const attribute of ["loading", "fetchpriority", "decoding"]) {
        expect(
          element.hasAttribute(attribute),
          `${attribute} landed on the box, where it does nothing`,
        ).toBe(false);
        expect(markImage(element).hasAttribute(attribute)).toBe(true);
      }
    }
  });

  it("changes nothing about which Mark is placed, or how big it is", () => {
    // The hints are about the fetch, never about the drawing — the box
    // arithmetic and the selection matrix are untouched by them.
    const plain = rendered(render(Logo, { layout: "stacked", on: "dark", size: 32 }));
    const hinted = rendered(
      render(Logo, {
        layout: "stacked",
        on: "dark",
        size: 32,
        loading: "lazy",
        fetchpriority: "high",
      }),
    );

    expect(hinted.getAttribute("data-logo-mark")).toBe(plain.getAttribute("data-logo-mark"));
    expect(styleOf(hinted, "padding")).toBe(styleOf(plain, "padding"));
    expect(markImage(hinted).getAttribute("width")).toBe(markImage(plain).getAttribute("width"));
    expect(markImage(hinted).getAttribute("height")).toBe(markImage(plain).getAttribute("height"));
    expect(classes(hinted)).toEqual(classes(plain));
  });
});
