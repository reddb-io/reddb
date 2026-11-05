// The lazy chart engine (wave 5D). A chart's figure — its accessible name, its
// Density-sized box and its rhythm probe — renders everywhere, the server
// included; LayerChart and its stylesheets load only when the figure nears the
// viewport, through a dynamic import the bundler splits into its own chunk.
// A page with no chart in view never downloads the engine, and its CSS never
// blocks the first paint.

import type { Component } from "svelte";

/** One chart engine: the LayerChart drawing, loaded once and shared by every instance. */
export interface ChartEngine<Props extends Record<string, any>> {
  /** The loaded drawing, or `null` until `load()` resolves. */
  readonly current: Component<Props> | null;
  /** Start (or join) the dynamic import; resolves once `current` is set. */
  load(): Promise<void>;
}

export function chartEngine<Props extends Record<string, any>>(
  importer: () => Promise<{ default: Component<Props> }>,
): ChartEngine<Props> {
  let current = $state.raw<Component<Props> | null>(null);
  let pending: Promise<void> | undefined;
  return {
    get current() {
      return current;
    },
    load() {
      pending ??= importer().then((module) => {
        current = module.default;
      });
      return pending;
    },
  };
}

export { CHART_LOAD_MARGIN, whenNear, type ChartLoading } from "./chart-loading";
import { whenNear, type ChartLoading } from "./chart-loading";

/**
 * Wire one chart instance: returns whether its engine should render, flipping
 * once the figure is near (or at mount, for `eager`) and starting the load.
 */
export function lazyChart<Props extends Record<string, any>>(
  engine: ChartEngine<Props>,
  figure: () => Element | undefined,
  loading: () => ChartLoading,
) {
  let wanted = $state(false);
  $effect(() => {
    const element = figure();
    if (!element || wanted) return;
    const start = (): void => {
      wanted = true;
      void engine.load();
    };
    if (loading() === "eager") {
      start();
      return;
    }
    return whenNear(element, start);
  });
  return {
    /** The engine to render, once wanted and loaded; `null` renders the placeholder. */
    get Engine(): Component<Props> | null {
      return wanted ? engine.current : null;
    },
  };
}
