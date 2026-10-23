export interface PanZoomView { x: number; y: number; scale: number }
export interface ViewportSize { width: number; height: number }
export function checkView(view: PanZoomView, min: number, max: number): PanZoomView {
  if (![view.x, view.y, view.scale, min, max].every(Number.isFinite) || min <= 0 || max < min || view.scale <= 0)
    throw new Error("PanZoomViewport: expected finite coordinates and positive ordered scale limits");
  return { ...view, scale: Math.max(min, Math.min(max, view.scale)) };
}
export function fitView(content: ViewportSize, viewport: ViewportSize, min: number, max: number): PanZoomView {
  if (![content.width, content.height, viewport.width, viewport.height].every(value => Number.isFinite(value) && value > 0))
    throw new Error("PanZoomViewport: dimensions must be finite and positive");
  const scale = checkView({ x: 0, y: 0, scale: Math.min(viewport.width / content.width, viewport.height / content.height) }, min, max).scale;
  return { x: (viewport.width - content.width * scale) / 2, y: (viewport.height - content.height * scale) / 2, scale };
}
/** Preserve the content coordinate beneath an anchor while the scale changes. */
export function zoomView(view: PanZoomView, factor: number, anchor: {x:number;y:number}, min: number, max: number): PanZoomView {
  if (!Number.isFinite(factor) || factor <= 0 || !Number.isFinite(anchor.x) || !Number.isFinite(anchor.y))
    throw new Error("PanZoomViewport: invalid zoom anchor or factor");
  const scale = checkView({ ...view, scale: view.scale * factor }, min, max).scale;
  const ratio = scale / view.scale;
  return { x: anchor.x - (anchor.x - view.x) * ratio, y: anchor.y - (anchor.y - view.y) * ratio, scale };
}

/** Canonical operations passed to caller-owned viewport controls. */
export interface PanZoomControls { zoomIn:()=>void;zoomOut:()=>void;reset:()=>void;view:PanZoomView; }
