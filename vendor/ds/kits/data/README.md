# Data Kit

## TerminalSession

Import `TerminalSession` and its adapter types from
`@reddb-io/design-system/data/TerminalSession`. Supply `label`, a browser-only
`engine(container, options)` factory and an optional plain-text `transcript`.
The factory dynamically imports your engine and its CSS and returns `write`,
`clear`, `focus`, `fit`, `update` and `dispose`. It must release partial work on
`options.signal` abort or factory failure; late resolved instances are disposed by
the component. `dispose` must be idempotent. Adapters provide their own accessible
input using `options.label` and must not focus it automatically.

`onsessionchange` receives the mounted handle, then `null` when it is replaced or
disposed. Send transport output to `handle.write`; forward `oninteraction` data
bytes and resize dimensions to your own transport. Connection, credentials, PTY,
command history, output buffering and reconnect policy belong to the application.
`readOnly` suppresses forwarding and updates the adapter. The component passes
computed DS typography and palette through `update`, calls `fit` as its container
or Density changes, retains the snapshot and offers retry on engine load failure.
`rows` reserves Density control-height multiples, not terminal cell rows. No engine
package, engine stylesheet or network connection is included by the Kit.

Lines and sparklines use smooth monotone curves by default; `curve="linear"`
retains straight segments. TimeSeries and MetricChart also accept
`variant="stacked-area"`: named contributions stack around zero, incomplete
timestamps remain gaps, and legend visibility recomputes the visible stack.
Summary and detail preserve the same curve, data and visible series. Value-axis
gutters are compact; legend toggles use filled/hollow markers without underlines.

The Data Kit is the Application Kit child for data-dense contracts. A consumer
that routes `data` also receives `app` and `base` through Kit inheritance.

Components are imported through individual subpaths:

```ts
import Table, {
  applyChanges,
  type TableChange,
  type TableColumn,
} from "@reddb-io/design-system/data/Table";
```

There is intentionally no Data barrel. Optional engines belong only to the
component modules that use them, while CSS files are the Kit's only declared
side effects.

| Subpath                                              | Optional peers it needs                                                                 |
| ---------------------------------------------------- | --------------------------------------------------------------------------------------- |
| `TimeSeries`, `Sparkline`, `MetricChart`                | `layerchart`, `d3-shape`                                                                |
| `Histogram`, `Heatmap`                                | `layerchart`                                                                            |
| `Graph`                                              | `graphology`, `@aflsolutions/graphology-communities-leiden`; `sigma` for the canvas engine |
| `KpiCard`, `BreakdownBar`, `Table`, `TreeView`, `JsonTree`, `XmlTree`, `DocumentViewer`, `HexViewer`, `Diff`, `CodeField` | none                                           |

## Sparkline presentations

Sparkline defaults to a line. Set `variant="area"` for a fill to zero or
`variant="bar"` for one vertical column per observation. Area and column
scales include zero; negative columns diverge below it. Line and area respect
`curve="smooth" | "linear"`. All presentations keep the same lazy engine,
Series Roles, accessible label and Density-relative height.

```svelte
<Sparkline values={history} variant="area" label="Request history" />
<Sparkline values={history} variant="bar" label="Jobs per interval" />
```

## Charts and KPI cards

`TimeSeries`, `Histogram`, `Sparkline`, and `Heatmap` are LayerChart v2 SVG
compositions. Each accepts explicit `width` and `height` values for deterministic
SSR/prerender output, and each maps LayerChart's color variables to the active
DS Theme and Color Scheme tokens. Importing one of these chart subpaths requires
the optional `layerchart` peer. `TimeSeries`, `Sparkline` and `MetricChart`
also require `d3-shape` for interpolation; install both with
`pnpm add layerchart d3-shape`. These peers load only with their chart engines.

```ts
import TimeSeries from "@reddb-io/design-system/data/TimeSeries";
import Histogram, { histogramBins } from "@reddb-io/design-system/data/Histogram";
```

`KpiCard` is DS-owned and has no chart-engine import. Its optional `sparkline`
snippet is caller-supplied, so a consumer importing only the card does not ship
LayerChart; a consumer that wants a trend can explicitly compose the `Sparkline`
subpath inside it.

### Comparable time series

`TimeSeries` also accepts named datasets, each `{ id, label, data, series? }`.
The same value and time axes serve every dataset; choose explicit Series Roles
when a metric must retain its colour across views. Named datasets show a legend
and shared sample readout by default (`interactive={false}` opts out).

Legend buttons toggle visibility by stable id without changing the shared ranges.
Move the pointer, tap, or focus the chart and use Arrow keys, Home and End to
inspect actual timestamps. The readout reports missing samples explicitly.
`formatX` formats the time axis and readout. `valueDomain={[0, 100]}` fixes
percentage comparisons to their full capacity; explicit bounds remain exact
and area bounds must contain zero. The engine stays lazy and the legend
and reserved plot height are present before it loads.

`variant="bar"` draws stacked temporal columns, with positive and negative
contributions stacked separately around zero. Missing samples stay absent.
`presentation="minimal"` removes axes, grid and their gutters. `size="sm" |
"md" | "lg"` reserves three/six/ten Density control rows, shared with
Histogram; an explicit `height` wins. Existing defaults stay unchanged.
`bind:hiddenSeries` shares hidden dataset ids with another view; legend actions
also report `onhiddenserieschange`.

## MetricChart and BreakdownBar

MetricChart composes TimeSeries, Dialog and Table around named `data` and a
visible `label`. The default summary includes a minimal stacked plot, a
headline and series totals. Expand opens axes, sample exploration and sortable
minimum/maximum/average/total statistics over the same observed samples. Missing
measurements read as unavailable. Series ids, colours and visibility survive
the transition and period refreshes. Set `presentation="full"` for an ordinary
chart card, and `detailHref` for a native application-owned Explore link.

`aggregate="total"` suits counter samples. `average` weights observed samples;
`last` sums the last observed value of each series. The headline is independent
of visibility. The application owns which aggregation makes sense. `format`
applies to every numerical reading. Optional `expandIcon` takes a consumer
glyph through Base Icon; otherwise the action visibly says Expand.

```svelte
<MetricChart label="Spend" data={costSeries} format={money} />
<MetricChart label="Requests" data={requestSeries} presentation="full" size="md" detailHref="/usage/requests" />
```

BreakdownBar takes `items` (`id`, `label`, non-negative `value`, optional
`series`). Exact proportions retain all groups; zeroes never get fabricated
width. Its labelled controls remain large enough to choose tiny contributions.
Bind `selected`, or handle `onselectedchange`, to filter an application-owned
table or detail view. Selection never changes the denominator or group widths.
Use explicit Series Roles to keep group identity across related charts.

KpiCard keeps its optional caller-imported Sparkline seam and its zero chart
engine boundary. The compact layout aligns title/value with the trend and
puts signed delta, direction arrow and visible comparison context below them.
`description` adds visible context; without it a delta shows `deltaLabel`.
Neutral cards use the raised surface without a default shadow, and polarity
continues to determine whether a delta means improvement or regression.

## Table

`Table` accepts caller-owned `rows` and a column model. Sorting, per-column
filters, pagination, fixed-row virtualization, column visibility, and roving
keyboard cell focus are local view state. Set `editable` to opt into the
columns marked `editable: true`; each edit stages an immutable `TableChange[]`
through `onChanges`. The component never commits data or creates a store — the
consumer can inspect, discard, or materialize the change-set with
`applyChanges`.

The named helpers exported beside the component are pure and have no render or
DOM dependency: `sortRows`, `filterRows`, `paginateRows`, `virtualWindow`,
`stageChange`, `valueWithChanges`, and `applyChanges`.

## TreeView

`TreeView` renders a labelled WAI-ARIA tree with roving focus, arrow-key
navigation, Home/End, and accumulating typeahead across visible nodes. Expansion
and selection can be local or caller-controlled through `expanded` / `onExpanded`
and `selected` / `onSelected`.

Pass `onRename` to expose the rename action (also available with `F2`). Pass
`onReorder` to expose drag-and-drop reordering. Both callbacks report proposed
changes without mutating the caller's nodes; `renamable` and `reorderable` can be
booleans or per-node predicates.

The component subpath also exports the pure generic path builder:

```ts
import TreeView, {
  buildPathTree,
  type TreeNode,
  type TreeReorder,
} from "@reddb-io/design-system/data/TreeView";

const nodes = buildPathTree({
  "src/index.ts": { bytes: 120 },
  "src/components/Button.svelte": { bytes: 840 },
});
```

`buildPathTree` splits record keys on `/` (or an explicitly supplied separator),
creates missing ancestors, naturally sorts every sibling set, and retains each
generic value on the node for its complete input path.

## JsonTree and XmlTree

`JsonTree` and `XmlTree` are read-only inspectors over the `TreeView`
interaction contract — same roving focus, keyboard navigation, and controlled
`expanded` / `onExpanded` and `selected` — with rename and reorder turned off.
`JsonTree` takes any JSON-compatible `value`; `XmlTree` takes XML source as a
string and parses it without a parser dependency, throwing a `SyntaxError` for
input that is not well formed or does not have exactly one root element.

Every node id is its dotted path (`items.0.name`; a scalar root is `$`), and
`onPath` receives that path when a node is selected. `buildJsonTree`,
`buildXmlTree`, and `dottedPath` are exported beside each component.

## DocumentViewer

`DocumentViewer` is master-detail browsing over caller-owned `documents`
(plain records). The list side has a search filter over titles, subtitles, and
every field; the detail side switches between `body`, `fields`, and `raw`
modes. Each part is read from the first conventional field that holds text:
the title from `title`/`name`/`label`/`subject` (else `Document N`), the id
from `id`/`_id`/`key` (else the index), the subtitle from
`subtitle`/`description`/`summary` (else a preview of the body), and the body
from `body`/`content`/`text`/`markdown` (else pretty JSON).

Selection and mode are local by default and controllable through `selected` /
`onSelected` (an index or an extracted id; `null` selects nothing) and `mode` /
`onMode`. The pure extraction helpers are exported beside it:
`extractDocumentSummary`, `filterDocuments`, `documentBody`, `documentFields`,
`documentRaw`, and `formatDocumentValue`.

## HexViewer

`HexViewer` renders bytes as an offset / hex / ASCII table with fixed-width
columns. `value` is an `ArrayBuffer`, a `Uint8Array`, or an array of integers
from 0 to 255; `bytesPerRow` defaults to 16. Offsets are at least eight hex
digits, and bytes outside printable ASCII show as `.`. The pure `hexBytes` and
`formatHex` helpers are exported beside it and throw a `RangeError` on
out-of-range bytes or a non-positive `bytesPerRow`.

## Diff

`Diff` computes the comparison itself, unlike the Application Kit's `Diff`,
which renders rows the caller already computed. Pass `before` and `after`:

- `mode="fields"` (default) flattens both values to dotted leaf paths and
  reports each path as added, removed, changed, or unchanged.
- `mode="lines"` compares the two strings line by line with LCS alignment,
  pairing adjacent removal and addition blocks as changes.

A polite live summary counts each kind above the entries. Every entry carries a
visible `+`, `−`, or `~` marker and an announced kind, so the Theme's success,
danger, and warning feedback roles are never the only cue. `showUnchanged`
(default `true`) hides unchanged entries when set to `false`. `diffFields`,
`diffLines`, and `formatDiffValue` are exported beside it.

```ts
import Diff, { diffFields } from "@reddb-io/design-system/data/Diff";
```

## CodeField

`CodeField` is a labelled text control — a single-line input, or with `gutter`
a line-numbered textarea of `rows` rows — whose value is mirrored by a token
highlight layer. `value` is bindable and `onValue` reports every edit.

Syntax is caller-owned through `grammar`, a `CodeFieldGrammar` with
`tokenize` and optional `completionAt` / `complete`. Without one, the field uses
the exported reference `variableTokenGrammar(resolved)`, which highlights
`{{name}}` tokens as resolved or unresolved against `resolved` (a list, a
`Set`, or a predicate).

When `suggestions` are supplied, the control is a combobox: the grammar's
completion point filters them by prefix, arrow keys move through the listbox,
Enter or Tab inserts the active suggestion, and Escape closes it. `help`,
`required`, `disabled`, `name`, and `placeholder` apply to the native control.

```ts
import CodeField, { variableTokenGrammar } from "@reddb-io/design-system/data/CodeField";
```

## Graph

`Graph` renders caller-owned `data` (`nodes` and `edges`, each carrying opaque
`metadata`) as an accessible SVG first. After mount it imports the Sigma canvas
engine lazily and hides the SVG once that engine is ready; `renderer="svg"`
keeps the SVG only, and a failed engine load leaves it in place.

Communities are detected with Leiden unless any node declares `community`, and
`collapsedCommunities` starts named communities as single supernodes. Layout is
a deterministic sparse-stress arrangement seeded by `layoutSeed` (`force` and
`radial` remain as legacy aliases of `layout`). `searchable` adds a node
search; focus is local or controlled through `focusedNode` / `onFocusedNode`,
and `onInteraction` receives node and edge clicks and keyboard focus with the
caller's metadata intact. Node colours are semantic DS tokens only.

Focusing a node plays a short pulse in both renderers; under
`prefers-reduced-motion: reduce` the pulse is skipped and the focused state is
held without animation.

### Minimum-configuration trends

Sparkline fills its container when `width` is omitted; pass a number for a fixed drawing. Its default height follows Density, including its server placeholder. KpiCard can therefore accept a Sparkline snippet without descendant sizing corrections.

## Charts, timing and diagrams

All new visualizations live beside the existing charts, with one import each:

| Component | Purpose | Build/client peers |
| --- | --- | --- |
| `Sankey` | Weighted, acyclic request and outcome flows | `layerchart` |
| `TraceWaterfall` | Request spans, concurrency, retries and selected attributes | `layerchart` |
| `Gantt` | Time intervals, hierarchy, progress and dependency references | `layerchart` |
| `MatrixHeatmap` | Generic row × column intensity; null/missing differs from zero | `layerchart` |
| `Quadrant`, `Scatter` | Two-dimensional observations and optional trend | `layerchart` |
| `Radar` | Explicit axis bounds and incomplete-series gaps | `layerchart` |
| `Treemap` | Hierarchical leaf-weighted area | `layerchart`, `d3-hierarchy` |
| `Waterfall` | Signed contributions, totals and subtotals | `layerchart` |
| `SchemaDiagram`, `EntityRelationship` | Field-aware relationships, constraints, cardinality and grouping | none |
| `Diagram`, `SequenceDiagram` | Declarative flows, architecture and ordered messages | none |
| `ExplodedLayers` | Isometric architectural layers using ordinary SVG | none |

The original `Heatmap` remains a calendar. `MatrixHeatmap` is its generic
matrix counterpart. `Waterfall` is an accumulated-value chart; `TraceWaterfall`
is the temporal APM view.

Every figure has `label`, optional `description`, `detailHref`, a complete
Explore data table and `size="sm" | "md" | "lg"`. Height follows Density;
explicit `width`/`height` override it. Small containers keep plots inside a
keyboard-scrollable region instead of shrinking labels to illegibility.
Radar and Sankey assign Series Roles automatically when omitted; explicit roles
keep application-owned color identities stable when ordering changes.
Selection is `bind:selected` plus `onselectedchange(id)`. `interactive={false}`
removes activation while retaining data and SVG titles. No component fetches
telemetry or infers units, aggregation or production status.

```svelte
<script lang="ts">
  import Sankey, { type SankeyData } from "@reddb-io/design-system/data/Sankey";
  import TraceWaterfall from "@reddb-io/design-system/data/TraceWaterfall";
  let selected = $state("");
  const ms = (v: number) => `${v} ms`;
  const spans = [{ id: "request", label: "POST /query", start: 0, end: 245 }];
  const flow: SankeyData = {
    nodes: [{ id: "api", label: "API" }, { id: "ok", label: "Success" }],
    links: [{ id: "completed", label: "Completed", source: "api", target: "ok", value: 900 }],
  };
</script>
<Sankey data={flow} label="Request outcomes" bind:selected />
<TraceWaterfall data={spans} label="Selected request" formatTime={ms} formatDuration={ms} />
```

Intervals use one caller-defined numeric unit for `start`/`end`. Set
`timeDomain` for fixed comparisons. Gantt and traces share hierarchy collapse,
zoom and position controls; selected attributes and links belong to the
application. Dependency references are drawn without modifying the schedule.

Schemas accept `entities` with fields, constraints, optional indexes/group and
optional `x`/`y` positions; `relations` reference entity and optional field ids.
`fieldLimit` limits the drawing, never the full data table. ER can label kinds
as `entity`, `aggregate` or `join`. The initial layout is deterministic and
read-oriented; these components are not interactive schema editors.

### Static marketing and documentation

New charts render SVG on the server, but importing a Svelte chart in a client
page still ships its runtime. To remove that cost, render it **only in a
server load or build step**, then insert the returned trusted DS markup:

```ts
// +page.server.ts
import Radar from "@reddb-io/design-system/data/Radar";
import { renderStaticVisualization } from "@reddb-io/design-system/data/static";
export function load() {
  return { figure: renderStaticVisualization(Radar, { axes, data, label: "Profiles" }) };
}
```

```svelte
<!-- +page.svelte: no chart import -->
<script lang="ts">let { data } = $props();</script>
{@html data.figure}
```

The output is HTML plus inline SVG using the surrounding DS stylesheet. It is
not a standalone `.svg` file: fonts and CSS variables remain supplied by the
page. Only use this seam with your trusted component output; application text
is escaped by Svelte. The static helper imports no diagram registry. Diagram
components need no chart peers; static charts need their peers only where they
are compiled/rendered, not in browser requests.

Inspect `/examples/request-monitor` and `/examples/static-visualizations` for
full compositions; each component also has an individual `/kit/...` route.

## Lab Harvest: lists, drawing navigation and geography

The labs supply interaction evidence, translated through the canonical DS
contracts (ADR 0035). These capabilities introduce no runtime dependency. Import
each through `@reddb-io/design-system/data/<Name>`; there is no Data barrel.

`VirtualList` takes `items`, a stable unique `rowKey`, a visible `label` and a row
snippet. `rowUnits` defaults to two Density control heights; `visibleRows` defaults
to six. Rows have a fixed height, including their content. Only the visible range,
overscan and any focused item remain mounted. Focused rows are retained while
scrolling; a removed focused item returns focus to the region. The region is
keyboard-scrollable, with native content controls and announced total/positions.
It is not a listbox or a variable-height measurement engine. Filtering, ordering,
fetching and selection stay with the application.

```svelte
<VirtualList label="Resources" items={filteredResources} rowKey={item => item.id}>
  {#snippet children(item, index)}
    <div class="flex h-full items-center gap-[var(--reddb-spatial-gap-md)]">
      <span>{index + 1}. {item.name}</span>
      <Button onclick={() => inspect(item.id)}>Inspect</Button>
    </div>
  {/snippet}
</VirtualList>
```

`PanZoomViewport` frames arbitrary caller-owned drawing content of logical
`contentWidth` × `contentHeight`. It fits initially and exposes `bind:view` with
`onviewchange`. Default controls zoom and fit; the `controls` snippet replaces
them. Native children keep their click, focus and input behavior. Arrow keys pan,
plus/minus zoom, Home fits, drag pans, two touches pinch and Control/Command plus
wheel zooms around the pointer. Ordinary wheel input still scrolls the page.
Transform changes are immediate, including under reduced motion. `size` uses the
same Density-relative height scale as the visualization family. Logical coordinates
are drawing data, not control sizing or another appearance axis.

`aspectRatio` optionally follows a drawing's width/height ratio while `size` caps
its height. GeoMap uses a 2:1 viewport so narrow screens keep the world in view
without excess vertical space. It retains the selected point's label first and
omits colliding labels using measured typography; all locations remain in its
textual list and all markers retain their complete accessible names.

`GeoMap` takes WGS84 `points`, optional labelled `routes` between point ids, and
optional application-owned `geography` (GeoJSON Polygon/MultiPolygon features).
It composes PanZoomViewport, uses an equirectangular projection and splits shortest
longitude crossings at the antimeridian. It does not measure real-world distances
or areas. `selected` and `onselectedchange` keep routing and detail application-owned.
An expandable textual list preserves exact locations and route descriptions.

No geographic dataset, tile server, map service or chart engine is imported by
GeoMap. The application supplies its geometry and `attribution`; the Showcase's
public-domain Natural Earth fixture is outside the release inputs. Inspect
`/examples/infrastructure-explorer`, `/kit/virtual-list`, `/kit/pan-zoom-viewport`
and `/kit/geo-map` for complete examples.

## Operational and spatial views

Import each component by its canonical `/data/<Component>` subpath; Sync selects
Svelte or React. No rendering engine is required for the new SVG/HTML surfaces.

- `StateGrid`: categorical entities with stable ids, state text and optional
  controlled selection. Static list semantics are the default.
- `WorkflowDiagram`: directed nodes, named input/output ports, bounded parallel
  channels and optional activity markers. `playing` is opt-in and stops when
  hidden, offscreen or under reduced motion. `focused` requests a camera target;
  `selected` and the inspector remain caller-owned.
- `EventLog`: retain a bounded tail before applying `query`, virtualize rows and
  follow new events only when requested. Navigating history suspends following.
  `onfollowingchange` lets the application reflect pause/resume intent.
- `QueueView`: ordered actual slots, including empty positions; no scheduling,
  inventory mutation or reordering is inferred.
- `MetricStrip`: canonical KpiCards with optional line, area or column sparklines.
- `IsometricBlock`, `ObjectStack`, `IsometricScene`: portable solid projected
  geometry. Stack captions retain the exact count and disclose representative
  limits. Nonintersecting scene entities support layer filtering, selection and
  focus. These contracts do not implement physics or arbitrary 3D rendering.
- `SceneViewport`: static accessible children, keyboard pan/zoom, spatial grid,
  focused bounds and an optional caller-supplied `engine` factory. The factory
  receives a host, label, data and AbortSignal and returns update/pause/destroy.
  Late handles are disposed, failures retain static content, and renderer code
  stays outside the default import graph. Renderers are decorative; native
  content and controls retain the accessible state.

`/examples/labs-workflow`, `labs-production`, `labs-inventory` and `labs-requests`
show the same composed behavior in both native flavors, including linked entity
selection, explicit verification/approval, replay, failures and resource transfer.

## Operational state summaries and schedule references

`StatusBreakdown` takes `items: { id, label, count, tone? }[]`. Counts are exact
non-negative integers; segments show their share of the total. Zero states remain
in the textual legend. `interactive` enables native filter buttons;
`selected` / `onselectedchange` are caller-controlled and never change counts.
The component does not infer progress, readiness, or approval from a count.

`Gantt` accepts `markers: { id, label, time, tone? }[]` for deadlines, observations
or other reference times and `windows: { id, label, start, end, intervalId? }[]`
for unavailable periods. All values use the same unit as `data`. An omitted
`intervalId` shades the full plot; a row window disappears when that row is
collapsed. References are clipped visually to the current time domain but retain
exact descriptions in the textual reference list. The component owns no clock,
rescheduling, prediction or lateness inference; the application supplies tones.

Set `showDetails={false}` when a shared App `EntityInspector` owns the selected
item's detail presentation. Use identical stable ids in `StateGrid`, `Gantt` and
inspector entities. Filtering a collection need not clear its inspected entity.
`/examples/operational-planning` demonstrates both native flavors with preview,
apply and reset, independent execution/approval, and keyboard selection. No new
runtime dependency or rendering engine is required by these capabilities.

### Scene labels and following

`SceneViewport` accepts `annotations: { id, x, y, label, description? }[]` in
logical scene coordinates. Labels stay constant-size through zoom, clamp to the
viewport, and omit collisions/off-scene anchors. The selected label has priority;
a focused label remains reachable. Provide a list or inspector for every entity,
including those whose label is omitted from the scene. `selected` and
`onannotationselect` keep inspection caller-owned.

`follow` supplies moving target bounds; `following` and `onfollowingchange`
control camera following independently of inspection. Drag, pinch, wheel zoom,
keyboard navigation, focus reveal and Fit suspend following with reason
`navigate`. Resume is explicit. Missing targets report `unavailable`; reduced
motion disables continuous following and reports `reduced-motion`. One-off
`focus` and manual pan/zoom remain available. No interpolated camera motion or
3D engine is included. `followLabel`, `pauseFollowLabel` and `reducedMotionLabel`
localize the controls. Omit `follow` to retain the original camera-only surface.

The underlying `PanZoomViewport` exposes an unscaled `overlay(view)` projection
and `onnavigate` for compositions that own their own camera UI. Initial fitting
never reports manual navigation. See `/examples/entity-follow` for both flavors.
