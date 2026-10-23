import { fitView, type PanZoomView } from "./pan-zoom";
import { observeActivity } from "@reddb-io/design-system/base";
/** Logical drawing bounds for an application-requested camera focus. */
export interface SceneBounds {
  x: number;
  y: number;
  width: number;
  height: number;
}
/** Optional renderer stays outside the default graph and owns its cleanup. */
export interface SceneEngine {
  update(data: unknown): void;
  pause(paused: boolean): void;
  destroy(): void;
}
export interface SceneEngineContext {
  label: string;
  data: unknown;
  signal: AbortSignal;
}
export type SceneEngineFactory = (
  host: HTMLElement,
  context: SceneEngineContext,
) => SceneEngine | Promise<SceneEngine>;
export function focusView(
  target: SceneBounds,
  viewport: { width: number; height: number },
): PanZoomView {
  if (
    ![target.x, target.y, target.width, target.height].every(Number.isFinite) ||
    target.width <= 0 ||
    target.height <= 0
  )
    throw new Error("SceneViewport: focus requires finite positive bounds");
  const fit = fitView(target, viewport, 0.05, 8);
  return {
    ...fit,
    x: fit.x - target.x * fit.scale,
    y: fit.y - target.y * fit.scale,
  };
}
/** Camera state is structural: unrelated parent renders cannot reset a paused view. */
export function mountSceneCamera(
  host: HTMLElement,
  changed: (view: PanZoomView) => void,
) {
  const viewport = host.querySelector<HTMLElement>(
    "[data-pan-zoom] [role=region]",
  );
  let target: SceneBounds | undefined,
    overviewKey: string | undefined,
    followKey: string | undefined,
    active = false;
  let lastReason: "unavailable" | "reduced-motion" | undefined;
  const key = (bounds: SceneBounds) =>
    [bounds.x, bounds.y, bounds.width, bounds.height].join(",");
  const paint = () => {
    if (!viewport || !target) return;
    const box = viewport.getBoundingClientRect();
    if (box.width && box.height) changed(focusView(target, box));
  };
  const observer =
    typeof ResizeObserver === "function"
      ? new ResizeObserver(paint)
      : undefined;
  if (viewport) observer?.observe(viewport);
  return {
    update(
      overview: SceneBounds,
      follow: SceneBounds | null | undefined,
      following: boolean,
      reducedMotion: boolean,
    ) {
      const reduced =
        reducedMotion ||
        Boolean(
          host.ownerDocument.defaultView?.matchMedia?.(
            "(prefers-reduced-motion: reduce)",
          ).matches,
        );
      const reason = following
        ? reduced
          ? "reduced-motion"
          : !follow
            ? "unavailable"
            : undefined
        : undefined;
      const nextActive = Boolean(following && follow && !reduced),
        nextOverview = key(overview),
        nextFollow = follow ? key(follow) : undefined;
      if (
        nextActive &&
        (nextFollow !== followKey || !active || nextOverview !== overviewKey)
      ) {
        target = { ...follow! };
        paint();
      } else if (!nextActive && nextOverview !== overviewKey) {
        target = { ...overview };
        paint();
      }
      overviewKey = nextOverview;
      followKey = nextFollow;
      active = nextActive;
      const report = reason !== lastReason ? reason : undefined;
      lastReason = reason;
      return report;
    },
    destroy() {
      observer?.disconnect();
    },
  };
}
/** Late async handles are disposed; no engine is imported by this module. */
export function mountSceneEngine(
  host: HTMLElement,
  factory: SceneEngineFactory,
  label: string,
  initial: unknown,
  status: (error: string | null) => void,
) {
  let data = initial,
    handle: SceneEngine | undefined,
    disposed = false,
    requested = false,
    active = false;
  const abort = new AbortController();
  const start = () => {
    if (requested || disposed) return;
    requested = true;
    void Promise.resolve()
      .then(() =>
        disposed ? null : factory(host, { label, data, signal: abort.signal }),
      )
      .then((engine) => {
        if (!engine) return;
        if (disposed) {
          engine.destroy();
          return;
        }
        handle = engine;
        engine.update(data);
        engine.pause(!active);
        status(null);
      })
      .catch(() => {
        if (!disposed)
          status(
            "Scene renderer unavailable. The static view remains available.",
          );
      });
  };
  const stop = observeActivity(host, (allowed) => {
    active = allowed;
    if (allowed) start();
    if (handle) {
      if (allowed) handle.update(data);
      handle.pause(!allowed);
    }
  });
  return {
    update(next: unknown) {
      data = next;
      if (active) handle?.update(data);
    },
    destroy() {
      disposed = true;
      abort.abort();
      stop();
      handle?.destroy();
      host.replaceChildren();
    },
  };
}
