// Structured Data (ADR 0028): the JSON-LD a page hands to search engines.
//
// Base carries only what a Base component needs to emit its own entity — the
// safe serializer and the BreadcrumbList builder Breadcrumbs uses — because a
// Base component may not import a child Kit. The Marketing Kit builds the rest
// (Organization, WebSite, articles, FAQ, products) on this serializer and adds
// the `JsonLd` and `SeoHead` components.

/** The vocabulary every DS-built node speaks. */
export const SCHEMA_ORG = "https://schema.org";

/** One schema.org node: a `@type` plus its properties. */
export interface JsonLdNode {
  "@context"?: typeof SCHEMA_ORG;
  "@type": string;
  "@id"?: string;
  [property: string]: unknown;
}

/** A top-level JSON-LD document: one node, or a `@graph` of several. */
export type JsonLdDocument =
  | JsonLdNode
  | { "@context": typeof SCHEMA_ORG; "@graph": readonly JsonLdNode[] };

// Characters that could close the surrounding <script> element, open an HTML
// comment, or break a JavaScript string literal if the block is ever inlined.
// Their JSON escapes keep the parsed value identical.
const UNSAFE = /[<>&\u2028\u2029]/g;
const ESCAPES: Record<string, string> = {
  "<": "\\u003c",
  ">": "\\u003e",
  "&": "\\u0026",
  "\u2028": "\\u2028",
  "\u2029": "\\u2029",
};

/**
 * Serialize a JSON-LD document so it is safe inside `<script type="application/ld+json">`.
 * Every `<`, `>` and `&` becomes its JSON unicode escape, so caller text containing
 * `</script` or `<!--` can neither close the element nor open a comment, and
 * `JSON.parse` still returns the original value. `undefined` properties are dropped.
 */
export function serializeJsonLd(document: JsonLdDocument): string {
  return JSON.stringify(document).replace(UNSAFE, (character) => ESCAPES[character]!);
}

/** The complete `<script type="application/ld+json">` element for a document, serialized safely. */
export function jsonLdScript(document: JsonLdDocument): string {
  return `<script type="application/ld+json">${serializeJsonLd(document)}</script>`;
}

/** One crumb: the visible label and, except for the current page, its destination. */
export interface BreadcrumbListInput {
  label: string;
  href?: string;
}

/** Options for {@link breadcrumbList}. */
export interface BreadcrumbListOptions {
  /** Absolute origin (`https://example.com`) that resolves root-relative `href`s. */
  origin?: string;
}

/** Resolve `href` against `origin` when it is root-relative; absolute URLs pass through. */
export function absoluteUrl(href: string, origin?: string): string {
  if (/^[a-z][a-z\d+.-]*:/i.test(href) || origin === undefined) return href;
  return new URL(href, origin.endsWith("/") ? origin : `${origin}/`).href;
}

/** True when `url` is an absolute `http(s)` URL. */
export function isAbsoluteUrl(url: string): boolean {
  return /^https?:\/\/[^/\s]+/i.test(url);
}

/**
 * A schema.org `BreadcrumbList` from Breadcrumbs-shaped items, in trail order. Each item
 * becomes a `ListItem` with its 1-based `position`; an item without `href` (the current page)
 * omits `item`, which schema.org allows for the last crumb.
 */
export function breadcrumbList(
  items: readonly BreadcrumbListInput[],
  options: BreadcrumbListOptions = {},
): JsonLdNode {
  return {
    "@context": SCHEMA_ORG,
    "@type": "BreadcrumbList",
    itemListElement: items.map((crumb, index) => ({
      "@type": "ListItem",
      position: index + 1,
      name: crumb.label,
      ...(crumb.href === undefined ? {} : { item: absoluteUrl(crumb.href, options.origin) }),
    })),
  };
}
