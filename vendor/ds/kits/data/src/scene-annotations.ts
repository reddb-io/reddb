import type { PanZoomView } from "./pan-zoom";
/** A constant-size HTML label anchored in logical scene coordinates. */
export interface SceneAnnotation {
  /** Stable identity shared with a list or inspector. */
  id: string;
  /** Logical anchor coordinate, before camera transform. */
  x: number;
  /** Logical anchor coordinate, before camera transform. */
  y: number;
  /** Visible and accessible identity. */
  label: string;
  /** Optional compact secondary context. */
  description?: string;
}
export function sceneAnnotations(items: readonly SceneAnnotation[]) {
  const ids = new Set<string>();
  for (const item of items) {
    if (
      !item.id.trim() ||
      ids.has(item.id) ||
      ![item.x, item.y].every(Number.isFinite)
    )
      throw new Error(
        "SceneViewport: annotations require unique ids and finite anchors",
      );
    ids.add(item.id);
  }
  return items;
}
/** Project then clamp a label to its viewport. Off-scene anchors are omitted. */
export function projectAnnotation(
  point: { x: number; y: number },
  view: PanZoomView,
  box: { width: number; height: number },
  label: { width: number; height: number },
) {
  const x = view.x + point.x * view.scale,
    y = view.y + point.y * view.scale;
  return {
    x: Math.max(0, Math.min(box.width - label.width, x - label.width / 2)),
    y: Math.max(0, Math.min(box.height - label.height, y - label.height)),
    visible: x >= 0 && x <= box.width && y >= 0 && y <= box.height,
  };
}
/** Shared DOM measurement keeps labels unscaled and prevents occluded focus targets. */
export function mountSceneAnnotations(host: HTMLElement) {
  let view: PanZoomView = { x: 0, y: 0, scale: 1 };
  const viewport = host.querySelector<HTMLElement>(
    "[data-pan-zoom] [role=region]",
  )!;
  const layout = () => {
    if (!viewport) return;
    const box = { width: viewport.clientWidth, height: viewport.clientHeight };
    const occupied: { x: number; y: number; width: number; height: number }[] =
      [];
    const labels = [
      ...viewport.querySelectorAll<HTMLElement>("[data-scene-annotation]"),
    ];
    labels.sort(
      (a, b) =>
        Number(b.contains(b.ownerDocument.activeElement)) -
          Number(a.contains(a.ownerDocument.activeElement)) ||
        Number(b.dataset.selected === "true") -
          Number(a.dataset.selected === "true"),
    );
    for (const node of labels) {
      node.hidden = false;
      const size = { width: node.offsetWidth, height: node.offsetHeight };
      const position = projectAnnotation(
        { x: Number(node.dataset.x), y: Number(node.dataset.y) },
        view,
        box,
        size,
      );
      const collision = occupied.some(
        (other) =>
          position.x < other.x + other.width &&
          position.x + size.width > other.x &&
          position.y < other.y + other.height &&
          position.y + size.height > other.y,
      );
      node.hidden =
        (!position.visible || collision) &&
        !node.contains(node.ownerDocument.activeElement);
      node.style.left = `${position.x}px`;
      node.style.top = `${position.y}px`;
      if (!node.hidden) occupied.push({ ...position, ...size });
    }
  };
  const observer =
    typeof ResizeObserver === "function"
      ? new ResizeObserver(layout)
      : undefined;
  if (viewport) observer?.observe(viewport);
  const observed = new Set<HTMLElement>();
  function observeLabels() {
    const current = new Set(
      host.querySelectorAll<HTMLElement>("[data-scene-annotation]"),
    );
    for (const node of observed)
      if (!current.has(node)) {
        observer?.unobserve(node);
        observed.delete(node);
      }
    for (const node of current)
      if (!observed.has(node)) {
        observer?.observe(node);
        observed.add(node);
      }
  }
  // Font loading and 200% text change measured label geometry independently of the viewport.
  observeLabels();
  return {
    update(next: PanZoomView) {
      view = next;
      observeLabels();
      layout();
    },
    destroy() {
      observer?.disconnect();
    },
  };
}
