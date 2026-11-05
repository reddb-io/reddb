import { unique, type VisualizationItem, type VisualizationSeries } from "./visualization";

export interface ExplodedLayer extends VisualizationItem {}

export function explodedLayers(data: readonly ExplodedLayer[]) {
  unique(data, "Exploded layers");
  return data.map((layer, index) => ({
    ...layer,
    series: layer.series ?? ((index % 6) + 1) as VisualizationSeries,
  }));
}

/** Rounded isometric plates, fitted to the drawing rather than leaving an unused row. */
export function explodedLayout(data: readonly ExplodedLayer[], width: number, height: number, unit = 32) {
  const drawingWidth = Math.min(width, height * 2);
  const offset = (width - drawingWidth) / 2;
  const cx = offset + drawingWidth * .32;
  const inset = Math.min(unit * .625, height * .05);
  const thickness = Math.min(unit * .5, height * .05);
  const half = Math.min(drawingWidth * .26, (height - inset * 2 - thickness) / (1 + Math.max(0, data.length - 1) * .625));
  const depth = half * .5;
  const step = data.length > 1 ? (height - inset * 2 - depth * 2 - thickness) / (data.length - 1) : 0;
  const rx = Math.min(unit * .5, half * .08);
  const ry = rx * depth / half;
  const labelX = offset + drawingWidth * .67;
  const labelWidth = offset + drawingWidth - labelX - inset;

  return data.map((layer, index) => {
    const cy = data.length > 1 ? inset + depth + index * step : (height - thickness) / 2;
    const left = cx - half, right = cx + half;
    // Every corner is tangent to the same projected rectangle, including the rear tip.
    const top = `M${cx-rx},${cy-depth+ry} Q${cx},${cy-depth} ${cx+rx},${cy-depth+ry} L${right-rx},${cy-ry} Q${right},${cy} ${right-rx},${cy+ry} L${cx+rx},${cy+depth-ry} Q${cx},${cy+depth} ${cx-rx},${cy+depth-ry} L${left+rx},${cy+ry} Q${left},${cy} ${left+rx},${cy-ry} Z`;
    // Extrude the visible half of the rim, joining at each rounded corner's
    // true extremum. Extruding a whole second diamond leaves crescent-shaped gaps.
    const leftTip = left + rx / 2, rightTip = right - rx / 2;
    const frontY = cy + depth - ry / 2;
    const leftFace = `M${leftTip},${cy} Q${leftTip},${cy+ry/2} ${left+rx},${cy+ry} L${cx-rx},${cy+depth-ry} Q${cx-rx/2},${frontY} ${cx},${frontY} V${frontY+thickness} Q${cx-rx/2},${frontY+thickness} ${cx-rx},${cy+depth+thickness-ry} L${left+rx},${cy+thickness+ry} Q${leftTip},${cy+thickness+ry/2} ${leftTip},${cy+thickness} Z`;
    const rightFace = `M${cx},${frontY} Q${cx+rx/2},${frontY} ${cx+rx},${cy+depth-ry} L${right-rx},${cy+ry} Q${rightTip},${cy+ry/2} ${rightTip},${cy} V${cy+thickness} Q${rightTip},${cy+thickness+ry/2} ${right-rx},${cy+thickness+ry} L${cx+rx},${cy+depth+thickness-ry} Q${cx+rx/2},${frontY+thickness} ${cx},${frontY+thickness} Z`;
    const front = `M${leftTip},${cy} Q${leftTip},${cy+ry/2} ${left+rx},${cy+ry} L${cx-rx},${cy+depth-ry} Q${cx},${cy+depth} ${cx+rx},${cy+depth-ry} L${right-rx},${cy+ry} Q${rightTip},${cy+ry/2} ${rightTip},${cy} V${cy+thickness} Q${rightTip},${cy+thickness+ry/2} ${right-rx},${cy+thickness+ry} L${cx+rx},${cy+depth+thickness-ry} Q${cx},${cy+depth+thickness} ${cx-rx},${cy+depth+thickness-ry} L${left+rx},${cy+thickness+ry} Q${leftTip},${cy+thickness+ry/2} ${leftTip},${cy+thickness} Z`;
    return { ...layer, index, cy, left: leftTip, right: rightTip, depth, thickness, top, leftFace, rightFace, front, labelX, labelWidth };
  });
}

/** Wrap callouts without squeezing type or discarding their descriptions. */
export function layerLines(text: string, columns: number): string[] {
  const lines: string[] = [];
  for (const word of text.split(/\s+/).filter(Boolean)) {
    const last = lines.length - 1;
    if (last >= 0 && lines[last]!.length + word.length + 1 <= columns) lines[last] += ` ${word}`;
    else {
      // Long identifiers still fit within the callout column.
      for (let start = 0; start < word.length; start += columns) lines.push(word.slice(start, start + columns));
    }
  }
  return lines;
}
