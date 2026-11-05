/** Server/build-only entry. Import the chosen component separately; no registry pulls in other families. */
import type { Component } from "svelte";
import { render } from "svelte/server";

/**
 * Produce trusted DS figure HTML at build time or in a server load/endpoint.
 * Insert this HTML into a page without hydrating the visualization. It uses the
 * surrounding DS stylesheet and axes; it is not a self-contained SVG image.
 * The browser receives markup only, with no parser, layout engine or handlers.
 */
export function renderStaticVisualization<Props extends { interactive?: boolean }>(component: Component<Props>, props: Props): string {
  return render(component, { props: { ...props, interactive: false } }).body;
}
