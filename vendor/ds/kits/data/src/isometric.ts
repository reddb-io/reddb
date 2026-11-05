import { uniqueEntities } from "./operational";
import type { VisualizationSeries } from "./visualization";
/** Solid boxes in a projected scene; physical simulation remains caller-owned. */
export interface IsometricItem {
  id: string;
  label: string;
  x: number;
  y: number;
  z?: number;
  width?: number;
  depth?: number;
  height?: number;
  series?: VisualizationSeries;
  layer?: string;
}
export interface Point {
  x: number;
  y: number;
}
export function isoPoint(x: number, y: number, z: number): Point {
  return { x: (x - y) * 24, y: (x + y) * 12 - z * 24 };
}
export function solidBox(
  width: number,
  depth: number,
  height: number,
  x = 0,
  y = 0,
  z = 0,
) {
  if (
    ![width, depth, height, x, y, z].every(Number.isFinite) ||
    width <= 0 ||
    depth <= 0 ||
    height <= 0 ||
    z < 0
  )
    throw new Error(
      "Isometric geometry requires positive finite dimensions and nonnegative elevation",
    );
  const p = (dx: number, dy: number, dz: number) =>
      isoPoint(x + dx, y + dy, z + dz),
    points = (ps: Point[]) => ps.map((v) => `${v.x},${v.y}`).join(" ");
  const top = [
    p(0, 0, height),
    p(width, 0, height),
    p(width, depth, height),
    p(0, depth, height),
  ];
  const left = [
    p(0, depth, 0),
    p(width, depth, 0),
    p(width, depth, height),
    p(0, depth, height),
  ];
  const right = [
    p(width, 0, 0),
    p(width, depth, 0),
    p(width, depth, height),
    p(width, 0, height),
  ];
  return {
    top: points(top),
    left: points(left),
    right: points(right),
    points: [...top, ...left, ...right],
    center: p(width / 2, depth / 2, height),
  };
}
export function sceneGeometry(items: readonly IsometricItem[]) {
  uniqueEntities(items, "IsometricScene");
  const boxes = items
    .map((item) => ({
      ...item,
      ...solidBox(
        item.width ?? 2,
        item.depth ?? 2,
        item.height ?? 1,
        item.x,
        item.y,
        item.z ?? 0,
      ),
    }))
    .sort(
      (a, b) =>
        a.x + a.y - (b.x + b.y) ||
        (a.z ?? 0) - (b.z ?? 0) ||
        (a.id < b.id ? -1 : a.id > b.id ? 1 : 0),
    );
  const bounds = boxes.reduce(
    (extent, box) => {
      for (const point of box.points) {
        extent.left = Math.min(extent.left, point.x);
        extent.top = Math.min(extent.top, point.y);
        extent.right = Math.max(extent.right, point.x);
        extent.bottom = Math.max(extent.bottom, point.y);
      }
      return extent;
    },
    { left: 0, top: 0, right: 1, bottom: 1 },
  );
  const left = bounds.left - 24,
    top = bounds.top - 24,
    right = bounds.right + 24,
    bottom = bounds.bottom + 24;
  return {
    boxes,
    viewBox: `${left} ${top} ${right - left} ${bottom - top}`,
    width: right - left,
    height: bottom - top,
  };
}
export function stackItems(count: number, maxVisible: number) {
  if (!Number.isFinite(count) || count < 0 || !Number.isInteger(count))
    throw new Error("ObjectStack: count must be a nonnegative integer");
  const shown = Math.min(
    count,
    Math.max(
      1,
      Math.min(24, Math.floor(Number.isFinite(maxVisible) ? maxVisible : 8)),
    ),
  );
  return {
    shown,
    items: Array.from({ length: shown }, (_, i) => ({
      id: String(i),
      label: `Item ${i + 1}`,
      x: 0,
      y: 0,
      z: i * 0.5,
      width: 4,
      depth: 3,
      height: 0.5,
    })),
  };
}

/** Geometry, visible layers and camera target are shared by both native adapters. */
export function isometricView(
  items: readonly IsometricItem[],
  layer: string | null,
  focused: string | null,
) {
  const scene = sceneGeometry(
      layer === null ? items : items.filter((item) => item.layer === layer),
    ),
    origin = scene.viewBox.split(" ").map(Number),
    target = scene.boxes.find((box) => box.id === focused);
  const focus = target
    ? {
        x: Math.min(...target.points.map((p) => p.x)) - origin[0]! - 12,
        y: Math.min(...target.points.map((p) => p.y)) - origin[1]! - 12,
        width:
          Math.max(...target.points.map((p) => p.x)) -
          Math.min(...target.points.map((p) => p.x)) +
          24,
        height:
          Math.max(...target.points.map((p) => p.y)) -
          Math.min(...target.points.map((p) => p.y)) +
          24,
      }
    : null;
  return { scene, focus };
}
