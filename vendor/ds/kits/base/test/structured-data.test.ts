// Structured Data in Base (ADR 0028): the safe serializer and the
// BreadcrumbList that Breadcrumbs emits when, and only when, it is asked to.

import { describe, expect, it } from "vitest";
import {
  Breadcrumbs,
  absoluteUrl,
  breadcrumbList,
  isAbsoluteUrl,
  jsonLdScript,
  serializeJsonLd,
} from "@reddb-io/design-system/base";
import { render } from "./mount";

describe("serializeJsonLd", () => {
  it("escapes everything that could close the script element or open a comment", () => {
    const hostile = { "@type": "Thing", name: "</script><script>alert(1)</script><!-- & \u2028" };
    const text = serializeJsonLd(hostile);

    expect(text).not.toMatch(/<|>|&|\u2028/);
    expect(text.toLowerCase()).not.toContain("</script");
    expect(text).not.toContain("<!--");
    expect(JSON.parse(text)).toEqual(hostile);
  });

  it("wraps the document in one ld+json script element", () => {
    const script = jsonLdScript({ "@type": "Thing", name: "</script>" });

    expect(script.startsWith('<script type="application/ld+json">')).toBe(true);
    expect(script.match(/<\/script>/g)).toHaveLength(1);
  });
});

describe("breadcrumbList", () => {
  it("numbers the trail, resolves root-relative hrefs and lets the current crumb omit item", () => {
    expect(
      breadcrumbList(
        [
          { label: "Home", href: "/" },
          { label: "Journal", href: "/blog" },
          { label: "When you do not need RedDB" },
        ],
        { origin: "https://reddb.io" },
      ),
    ).toEqual({
      "@context": "https://schema.org",
      "@type": "BreadcrumbList",
      itemListElement: [
        { "@type": "ListItem", position: 1, name: "Home", item: "https://reddb.io/" },
        { "@type": "ListItem", position: 2, name: "Journal", item: "https://reddb.io/blog" },
        { "@type": "ListItem", position: 3, name: "When you do not need RedDB" },
      ],
    });
  });

  it("leaves absolute URLs alone and knows one when it sees one", () => {
    expect(absoluteUrl("https://github.com/reddb-io", "https://reddb.io")).toBe("https://github.com/reddb-io");
    expect(absoluteUrl("/blog")).toBe("/blog");
    expect(isAbsoluteUrl("https://reddb.io/blog")).toBe(true);
    expect(isAbsoluteUrl("/blog")).toBe(false);
  });
});

describe("Breadcrumbs structuredData", () => {
  const items = [
    { id: "home", label: "Home", href: "/" },
    { id: "journal", label: "Journal", href: "/blog" },
    { id: "post", label: "Post", current: true },
  ];

  it("emits nothing by default, so a page never describes the trail twice", () => {
    const root = render(Breadcrumbs, { items });
    expect(root.querySelector('script[type="application/ld+json"]')).toBeNull();
  });

  it("emits a BreadcrumbList with absolute URLs when given an origin", () => {
    const root = render(Breadcrumbs, { items, structuredData: { origin: "https://reddb.io" } });
    const script = root.querySelector('nav script[type="application/ld+json"]');
    const data = JSON.parse(script!.textContent!);

    expect(data["@type"]).toBe("BreadcrumbList");
    expect(data.itemListElement.map((item: { item?: string }) => item.item)).toEqual([
      "https://reddb.io/",
      "https://reddb.io/blog",
      undefined,
    ]);
  });
});
