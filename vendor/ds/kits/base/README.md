# Base Kit

## Numeric entry and rich choices

`NumberField` composes Field, Input and Button. Supply a visible `label`, bind
`value` (`number | null`), and optionally choose finite `min`, `max` and positive
`step` constraints. Native numeric entry owns keyboard stepping, decimal alignment,
validation and form submission. Invalid typed values stay visible. Both stepping
buttons share Input's `controlSize`; readonly and disabled fields cannot step.
`help`, `error` and `required` use Field's canonical accessible relationships.
Native form resets update the binding and call `onvaluechange` once. An external
null clears the field. Applications own persistence, quotas and validation copy.

`RadioGroup` retains its default list and native radio behavior. Set
`variant="cards"` for responsive bordered choices. Options accept `description`
for context or an unavailable reason; `optionMedia(option)` supplies noninteractive
media or metadata beside the canonical label. Do not nest links or buttons inside
a radio label. Labels name the choice; descriptions are associated separately.
The application still owns selection, business rules and form submission.

Dialog and Drawer accept `header`, a snippet receiving `{ close }`. It
replaces the visible title and standard close control; the required `title`
remains their accessible name. A custom header keeps the body in one scrolling
region. The Drawer showcase demonstrates summaries, member search,
shared-content tabs, previews, return navigation and a persistent footer.

The parent Kit every other Kit inherits, routed to every consumer (ADR 0004).
Its `kit.json` declares `audience: "*"` and no `parents` — it is the root of
the Kit graph, and every other Kit in `kits/` declares it as a parent.

## Why it exists

A component that every application needs — the Logo first — should not oblige
an application to pull a sibling Kit to reach it. So the Base Kit is delivered
by inheritance: `ds-sync` resolves a declared Kit's parents transitively, so a
consumer that declares **any** Kit receives this one too, and a consumer that
needs nothing else may declare `base` alone.

Nothing in the Sync knows the name `base`. "Routed to every consumer" holds
because every Kit declares the parent, which is checked on the DS side by
`scripts/producer/test/kits.test.ts` — a Kit added without that declaration
fails there instead of quietly reaching only the consumers that name it.

## What is in it

The universal **Alert**, **Notification**, **EmptyState**, **AspectRatio**, **ContentMask**, **Button**, **Card**, **CodeBlock**, **Container**, **ControlGroup**, **Divider**, **Kbd**, **Link**, **Stack**, **SkipLink**, **Dialog**, **Drawer**,
**AlertDialog**, **Tooltip**, **Label**, **Field**, **Fieldset**, **Form**, **Input**, **Textarea**,
**FileInput**, **OneTimeCodeInput**, **Select**, **Checkbox**, **RadioGroup**, **Switch**, **Slider**,
**Rating**, **ToggleButton**, **ToggleGroup**, **Progress**, **RadialProgress**, **Meter**,
**LoadingIndicator**, **Skeleton**, **Statistic**, **Countdown**, **Timeline**, **Calendar**, and
**RangeCalendar**, **DateField**, and **DateRangeField** contracts, and the **Logo** with the Marks it places.
`src/index.ts` carries the
`BASE_COMPONENTS` list, pinned against the files on disk by
`test/components.test.ts`, so a component added here cannot go unexported.

The bar for entry is higher than the application Kit's, not lower: a component
here reaches every audience, so it must be cross-audience by nature.

## The Button

Button preserves the platform instead of wrapping it in a smaller API. With no
`href` it renders a native `<button>` and passes native attributes and handlers
through; with `href` it renders a native `<a>`. Disabled and loading buttons use
the platform's disabled state. Because anchors have no equivalent, an inert
anchor withholds its destination and tab stop and exposes `aria-disabled`.
Loading also exposes `aria-busy` and keeps the label beside a decorative
spinner.

The public `variant`, `size`, and `block` axes are enumerated by
`BUTTON_VARIANTS` and `BUTTON_SIZES`. The exported `button` and `buttonSpinner`
variant functions are Extension Seams: a Product Application can compose the
canonical classes onto its own element or build an explicitly named local
specialization without copying Button's behavior. The seams are optional;
vendored source remains application-owned and may still be changed directly.

Base is Button's only canonical source and public export. A Product Application
that declares the Application Kit receives Base through the Kit's `parents`
edge and imports Button from the explicit Base subpath; the child Kit neither
shadows the name nor replaces its behavior.

## Link, Kbd, and CodeBlock

Link is always a native `<a>`. Its persistent underline distinguishes it from
surrounding prose without relying on colour, while its focus ring makes the
platform keyboard stop visible. Native destinations, relationships, download
behavior, and caller attributes pass through unchanged.

Kbd renders the platform's `<kbd>` semantics. A chord accepts its keys as a
list, renders one cap per key, and keeps the visual separator out of the
assistive-technology reading. Cap size follows the typography it sits in;
only the gap between caps follows the nearest Density scope.

CodeBlock preserves the caller's source exactly in native `<pre><code>`
semantics. Its keyboard-focusable copy affordance composes the canonical Base
Button and copies that exact source rather than reading normalized markup.
Language labels and source remain caller content; page layout stays outside
the component.

```svelte
<script lang="ts">
  import { CodeBlock, Kbd, Link } from "@reddb-io/design-system/base";
</script>

Read the <Link href="/guide">guide</Link>, then press <Kbd keys={["Ctrl", "K"]} />.

<CodeBlock language="TypeScript" code={'const ready = true;'} />
```

## Alert and Feedback Roles

Alert requires one DS-owned `feedback` meaning: `info`, `success`, `warning`, or
`danger`. Success and warning updates use the polite `status` role; danger is
an assertive `alert`, so errors reach assistive technology without relying on a
visual-only convention. Callers may provide a visible `title`, arbitrary
message content, native `<div>` attributes, and additional classes.

The exported `alert` function is the optional appearance Extension Seam. Its
classes name only `--reddb-color-feedback-*` semantics. Theme owns the current
Brand-material mapping behind those meanings, so a Brand Sync can replace a
temporary material without changing Alert's public API or behavioral tests.

```svelte
<script lang="ts">
  import { Alert } from "@reddb-io/design-system/base";
</script>

<Alert feedback="danger" title="Deployment failed">
  The previous release is still serving traffic.
</Alert>
```

## Notification and EmptyState

Notification composes Alert's polite/assertive Feedback Role semantics with a
canonical Button dismissal control. Its required title is always inside the
live announcement, the optional detail remains caller content, and dismissal
removes the transient surface and calls `ondismiss`. Placement, stacking, and
the decision to create or expire a notification remain consumer arrangement.

EmptyState is the canonical zero-data surface inherited by every Kit. Its
required title says what is empty; optional description, decorative media,
literal hint, and action snippets let the consumer explain why and supply its
own way out without turning page-specific business content into DS content.

```svelte
<script lang="ts">
  import { Button, EmptyState, Notification } from "@reddb-io/design-system/base";
</script>

<Notification feedback="success" title="Deployment complete">
  The new release is serving traffic.
</Notification>

<EmptyState title="No nodes yet" description="A node appears here as soon as one joins.">
  {#snippet actions()}<Button>Add a node</Button>{/snippet}
</EmptyState>
```

Their exported `notification` and `emptyState` functions are optional
appearance Extension Seams. Spatial values use Density roles, Feedback Role
colour stays Theme-owned through Alert, and neither component selects Theme,
Color Scheme, or Density for its caller.

## ScrollArea, Swap, AppearanceSwitch, and SectionHeading

ScrollArea is a named native overflow region with a guaranteed keyboard stop.
The caller owns its size constraint and content; the component owns overflow
direction, visible focus, and the Density-backed inset that makes contained
content readable at every local stop.

Swap composes ToggleButton rather than creating another pressed-state model.
Its stable `label` names the control while caller-owned `off` and `on` snippets
replace one another. The `swapped` state is bindable and the `onchange`
callback reports each native activation.

AppearanceSwitch composes Field and Select for one independent `theme`,
`color-scheme`, or `density` axis. Options remain caller-owned, the selected
value is applied to exactly one `data-*` attribute, and one storage key per
axis restores a valid value on mount. `root` and `storage` are injectable so a
consumer may target an appearance scope or persistence adapter without
coupling Base to an application store.

SectionHeading is the cross-audience section title. `level` selects native
outline depth from `h2` through `h6`; `size` selects visual weight separately.
Optional supporting copy, rule, and caller-owned actions do not make it a page
heading or a business-content assembly.

```svelte
<script lang="ts">
  import {
    AppearanceSwitch,
    ScrollArea,
    SectionHeading,
    Swap,
  } from "@reddb-io/design-system/base";

  const themes = [
    { value: "application", label: "Application" },
    { value: "marketing", label: "Marketing" },
  ];
</script>

<SectionHeading title="Deployments" level={3} size="lg" />
<ScrollArea label="Recent deployments" class="max-h-64">…</ScrollArea>
<Swap label="Show deployment detail">
  {#snippet off()}Summary{/snippet}
  {#snippet on()}Detail{/snippet}
</Swap>
<AppearanceSwitch label="Theme" axis="theme" options={themes} />
```

## Badge, Indicator, and StatusIndicator

Badge is a short text status marker that can sit inside running text. Its
`neutral`, `primary`, and `outline` variants are emphasis only: caller-owned
text is required and always rendered, so a background or border can never be
the whole message. Badge remains a non-interactive `<span>`; linked or pressed
behavior belongs to the corresponding native control wearing the exported
`badge` classes.

Indicator owns only logical marker placement. Its required `marker` snippet may
contain Badge, StatusIndicator, or consumer content at one of eight start/end
anchors. The anchored child keeps its native element, keyboard behavior, and
focus; Indicator adds no control or business status of its own.

StatusIndicator pairs a decorative mark with required status text. `showLabel`
may visually hide that text, but it remains screen-reader content and the mark
is always hidden from assistive technology. Its stable meanings are `neutral`
plus Alert's canonical `success`, `warning`, and `danger` Feedback Roles, so
the Theme mapping is composed rather than redefined.

```svelte
<script lang="ts">
  import { Badge, Button, Indicator, StatusIndicator } from "@reddb-io/design-system/base";
</script>

<p>The release is <Badge variant="primary">stable</Badge>.</p>

<StatusIndicator label="Available" status="success" />

<Indicator position="top-end">
  {#snippet marker()}
    <StatusIndicator label="3 warnings" status="warning" showLabel={false} />
  {/snippet}
  <Button>Review deployment</Button>
</Indicator>
```

The exported `badge`, `indicator`, and `statusIndicator` functions are optional
appearance Extension Seams. Their spatial roles resolve through the active
Density scope, and none selects Theme, Color Scheme, or Density for a consumer.

## Switch and neutral surfaces

Neutral Card now uses the raised surface with its quiet border and no default
shadow, separating a card from the canvas in the dark Color Scheme. Explicit
`raised` adds the complete border/shadow treatment. Feedback tones keep their
paired surfaces and copy. A continuous surface can explicitly use `bg-background`.

Switch separates its painted track and centred thumb from the native input's
hit area. Density controls their proportions, while a coarse pointer still
receives a target of at least 44px without enlarging the visual switch. Native
form semantics, label association, keyboard handling, validation, disabled and
reduced-motion behavior remain intact.

## Dialog, Drawer, AlertDialog, and Tooltip

Dialog also accepts `size="xl"`, using the DS wide container for expanded
charts and detail tables. Its existing sizes remain unchanged.

Dialog owns the complete modal lifecycle behind one Base API. Its visible
`title` labels the native dialog, an optional `description` is associated with
it, focus enters the modal and cannot Tab outside it, Escape and the visible
close control dismiss it, and focus returns to the invoking trigger. The
trigger's `triggerLabel` remains its accessible name when custom trigger
content is rendered. Dialog uses the platform's modal top layer (ADR 0017)
while keeping these observable behaviors in the DS contract.

`open` is bindable and `onopenchange` reports every open and close, whatever
caused it. Omit `triggerLabel` to render no trigger at all and open the Dialog
from a menu item, a command or a route with `bind:open`. A click outside the
surface dismisses it unless `dismissible={false}`; page scroll is locked while
any modal is open; `size` (`sm`, `md`, `lg`) sets its measure; and title and
description ids come from `$props.id()`, so they match between server and
client.

Drawer composes that lifecycle and anchors the same modal contract to the
`top`, `right`, `bottom`, or `left` edge. Its edge is structural; its inset,
colour, and motion remain token-backed, so Theme, Color Scheme, and nested
Density scopes stay caller-owned.

AlertDialog composes Dialog and Button for one destructive decision. It uses
the `alertdialog` role, focuses the safe action first, ignores outside clicks,
and restores invoking focus after cancel, confirmation, or Escape. The
confirmation wears the `danger` intent, never the Brand accent. Callers own the
consequence through `onconfirm` and all explanatory content; when `onconfirm`
returns a promise the confirmation shows its busy state and the dialog closes
only once the promise fulfils, staying open on rejection so the caller can
report why.

Tooltip is supplemental by construction. Its required `label` is the trigger's
complete accessible name; `content` supplies additional description and is
never the only carrier of what the control does. Keyboard focus and pointer
hover expose the same `role="tooltip"` content, blur and pointer exit close it,
and Escape dismisses it without moving focus. To describe an element you
already render — an icon Button, a link — pass a `child` snippet and spread
its `props` onto that element; it then owns its accessible name and `label` is
not needed.

All four capabilities expose token-only appearance seams. Drawer additionally
exports `DRAWER_SIDES`; AlertDialog exports its action-layout seam.
Their motion transitions include an explicit reduced-motion path and their
public showcase routes render across every Theme × Color Scheme × Density cell.

```svelte
<script lang="ts">
  import { AlertDialog, Dialog, Drawer, Tooltip } from "@reddb-io/design-system/base";
</script>

<Dialog triggerLabel="Edit deployment" title="Edit deployment">
  {#snippet trigger()}Edit{/snippet}
  <p>Caller-owned modal content.</p>
</Dialog>

<Drawer triggerLabel="Open filters" title="Filters" side="right">
  <p>Caller-owned filter controls.</p>
</Drawer>

<AlertDialog
  triggerLabel="Delete deployment"
  title="Delete deployment?"
  confirmLabel="Delete"
  onconfirm={deleteDeployment}
>
  <p>This cannot be undone.</p>
</AlertDialog>

<Tooltip label="Copy connection string" content="Copies the value to your clipboard">
  Copy
</Tooltip>

<!-- Controlled: no DS trigger, opened from anywhere. -->
<Dialog title="Rename collection" bind:open={renaming} size="sm">
  <Input aria-label="Name" bind:value={name} />
</Dialog>

<Tooltip content="Copies the value to your clipboard">
  {#snippet child({ props })}
    <Button {...props} variant="ghost" aria-label="Copy">…</Button>
  {/snippet}
</Tooltip>
```

## Popover and LinkPreview

Popover is the canonical interactive anchored overlay. It composes the Base
Button with Bits UI's non-modal dialog behavior: collision detection keeps the
surface inside the viewport, focus enters caller-owned content, Escape and an
outside interaction dismiss it, and focus returns to the trigger. Required
`triggerLabel` and `contentLabel` names keep both ends of the relationship
explicit; side and alignment are preferences that may flip to avoid a viewport
edge.

LinkPreview keeps its trigger a native link. Keyboard focus and pointer hover
open the same collision-aware preview, focus remains on the link, Escape closes
the preview, and navigation still works when preview behavior is unavailable.
Its content supplies context rather than actions, so it never introduces a
focus trap.

The exported `popover` appearance seam contains the shared token-only anchored
surface. `linkPreview` extends it instead of restating the surface contract.
Both keep Theme, Color Scheme, Contrast, and Density selection outside their
APIs; spatial values remain live Density roles in nested scopes.

```svelte
<script lang="ts">
  import { LinkPreview, Popover } from "@reddb-io/design-system/base";
</script>

<Popover triggerLabel="Open filters" contentLabel="Deployment filters">
  <button type="button">Apply filters</button>
</Popover>

<LinkPreview href="/clusters/primary" label="Primary cluster" previewLabel="Primary cluster preview">
  Healthy in sa-east-1
</LinkPreview>
```

## DropdownMenu and NavigationMenu

DropdownMenu is the canonical command menu over a Base Button. Its items may
be a flat list or named groups; Bits UI supplies the roving focus, typeahead,
selection, collision handling, Escape dismissal, and focus return. Link rows
remain native anchors, disabled rows stay perceivable and are skipped, and the
surface composes the exported `popover` appearance rather than restating an
anchored overlay.

NavigationMenu is the canonical list of destinations. Direct entries remain
native links while a section opens a Popover-composed surface of related links.
The top level roves in either horizontal or vertical orientation, current-page
state is carried by `aria-current`, and Escape dismisses an open section without
leaving focus behind.

Both capabilities expose only token-backed size and orientation seams. Theme,
Color Scheme, Contrast, and Density stay in the surrounding cascade, including
when either menu is mounted inside a nested appearance scope.

## Navbar, Breadcrumbs, Pagination, and Steps

The four wayfinding contracts keep current position in native semantics rather
than in colour. Navbar composes Base Link and Button into brand, links, and
actions regions; its responsive arrangement is selected in CSS, while its
collapsed disclosure restores focus on Escape. Breadcrumbs announces exactly
the caller-designated current page. Pagination gives every numeric destination
an explicit page label and marks the current page. Steps uses an ordered trail,
`aria-current="step"`, a visible completion check, and completion text.

Callers own destinations, labels, brand/action content, and page arrangement.
The exported `navbar`, `breadcrumbs`, `pagination`, and `steps` appearance seams
use live semantic colour and spatial roles without selecting Theme, Color
Scheme, Contrast, or Density.

```svelte
<script lang="ts">
  import { Breadcrumbs, Navbar, Pagination, Steps } from "@reddb-io/design-system/base";
</script>

<Navbar links={[{ id: "home", label: "Home", href: "/", current: true }]} />
<Breadcrumbs items={[{ id: "home", label: "Home", current: true }]} />
<Pagination pages={[{ page: 1, href: "?page=1", current: true }]} />
<Steps items={[{ id: "review", label: "Review", state: "current" }]} />
```

```svelte
<script lang="ts">
  import { DropdownMenu, NavigationMenu } from "@reddb-io/design-system/base";
</script>

<DropdownMenu
  triggerLabel="Actions"
  contentLabel="Deployment actions"
  items={[{ id: "restart", label: "Restart", onselect: restart }]}
/>

<NavigationMenu
  label="Primary"
  items={[
    { id: "overview", label: "Overview", href: "/overview", active: true },
    {
      id: "learn",
      label: "Learn",
      links: [{ id: "docs", label: "Documentation", href: "/docs" }],
    },
  ]}
/>
```

## Container, Stack, and SkipLink

Container is the Base content boundary: full width until a canonical maximum,
centered, with an inline inset that names the Density axis's `inset-md` role.
Its width is structural and does not change by Density; its inset remains a CSS
variable reference, so a nested Density scope re-resolves it without changing
markup or selecting an axis in the component.

Stack puts its children in vertical flow and exposes exactly the `sm`, `md`,
and `lg` gap roles the Density axis ships. The `STACK_GAPS` enumeration and
exported `stack` appearance seam let a showcase or local specialization use the
same closed vocabulary without copying it.

SkipLink defaults to `#main-content` and the complete label “Skip to main
content”. It is visually hidden until keyboard focus, then becomes a token-
painted fixed control. Activating an in-page destination moves focus to it; the
document's real `<main id="main-content" tabindex="-1">` provides that target
without a router-specific URL. Native anchor attributes, a custom in-page
`href`, and caller-owned wording remain available.

```svelte
<script lang="ts">
  import { Container, SkipLink, Stack } from "@reddb-io/design-system/base";
</script>

<SkipLink />
<main id="main-content" tabindex="-1">
  <Container>
    <Stack gap="lg">
      <h1>Deployment</h1>
      <p>Bounded content in token-backed vertical flow.</p>
    </Stack>
  </Container>
</main>
```

## List, ListContainer, DescriptionList, and GridList

List owns native unordered or ordered collection semantics while its `sm`,
`md`, and `lg` gaps remain live Density roles. Items and their content stay
with the consumer, so native links and controls keep document focus order.

ListContainer composes the canonical Card contract around caller-owned list
content. It forwards Card's header, edge, and inset choices rather than
creating a second surface vocabulary. GridList similarly composes List, adding
only a one-to-four-column responsive arrangement; it never replaces the
underlying `<ul>` semantics.

DescriptionList accepts term/detail entries and renders one valid `<dt>` and
`<dd>` pair for each. Terms and details can be strings or caller-owned snippets,
so business content stays local while pairing cannot drift. All four
structures select no Theme, Color Scheme, Contrast, or active Density value.

```svelte
<script lang="ts">
  import {
    DescriptionList,
    GridList,
    List,
    ListContainer,
  } from "@reddb-io/design-system/base";

  const details = [
    { term: "Region", detail: "South America" },
    { term: "Owner", detail: "Platform team" },
  ];
</script>

<ListContainer title="Deployments">
  <List aria-label="Recent deployments"><li>Production</li></List>
</ListContainer>
<DescriptionList aria-label="Service details" items={details} />
<GridList columns={2} aria-label="Projects"><li>Design system</li></GridList>
```

## Calendar and RangeCalendar

Calendar is the canonical single-date grid used by later date fields and
pickers. RangeCalendar is its inclusive-range counterpart. Both compose Bits
UI's localized month model, roving date focus, arrow-key navigation, disabled
date handling, and polite selection announcements with canonical Base Button
month controls and one exported `calendar` appearance seam.

Selected dates expose `aria-selected` in addition to their visible treatment.
Range starts, middles, and ends also have distinct geometry, while the complete
range is announced in words. All geometry uses live Density roles; neither
component selects Theme, Color Scheme, or Density. Callers own locale, date
constraints, selected values, page arrangement, and business content.

```svelte
<script lang="ts">
  import { Calendar, RangeCalendar } from "@reddb-io/design-system/base";
</script>

<Calendar label="Deployment date" bind:value={deploymentDate} />
<RangeCalendar label="Deployment window" bind:value={deploymentWindow} minDays={2} />
```

## Card, Divider, and AspectRatio

Card is the universal bounded surface. Its optional header, body, and footer
share one `variant` and one `padding` selection, so their edges and insets
cannot drift apart. The `sm` and `md` paddings name live Density roles in every
section; nested scopes re-resolve them without a Card prop or attribute naming
the active stop. Header and footer markup is omitted when its content is absent.
The optional `busy` contract retains current content, publishes `aria-busy`,
and adds the canonical LoadingIndicator with caller-owned waiting language.

Divider renders the separator role together with its horizontal or vertical
`aria-orientation`. It is not a keyboard stop: callers get a perceivable
structural boundary without introducing an inert tab target. Its muted rule is
painted by the surrounding Theme and Color Scheme.

AspectRatio accepts any numeric width-to-height ratio, defaulting to 16:9. It
sets only the structural `aspect-ratio` property and lets caller-owned media,
links, and controls keep their native content and focus behavior. It selects no
Theme, Color Scheme, Contrast, or Density value.

```svelte
<script lang="ts">
  import { AspectRatio, Card, Divider } from "@reddb-io/design-system/base";
</script>

<Card title="Deployment" description="Healthy in sa-east-1">
  <AspectRatio ratio={4 / 3}>
    <img src={preview} alt="Deployment topology" />
  </AspectRatio>
  <Divider />
  <a href="/deployments/primary">Open deployment</a>
</Card>
```

## BrowserMockup, PhoneMockup, and WindowMockup

The three mockups are presentation containers for caller-owned content.
BrowserMockup adds a display-only address bar, PhoneMockup adds a decorative
phone body, and WindowMockup adds a display-only title bar. Each composes the
canonical AspectRatio boundary, exposes an overridable viewport ratio, and
leaves screenshots, live embeds, code samples, links, controls, accessible
names, and business language with the consumer.

Only the chrome is `aria-hidden`. The frame itself is presentational rather
than a landmark, control, or link, so content inside keeps its native role,
name, document order, and keyboard behavior. Fake addresses and titles are
plain text inside hidden chrome and can never be mistaken for real controls.

Their exported `browserMockup`, `phoneMockup`, and `windowMockup` appearance
seams use semantic colours and live Density roles. None selects Theme, Color
Scheme, Contrast, or Density, including inside a nested appearance scope.

```svelte
<script lang="ts">
  import { BrowserMockup, PhoneMockup, WindowMockup } from "@reddb-io/design-system/base";
</script>

<BrowserMockup address="https://example.test/deployments">
  <article aria-label="Deployment preview">...</article>
</BrowserMockup>

<PhoneMockup>
  <nav aria-label="Deployment queue">...</nav>
</PhoneMockup>

<WindowMockup title="Build output">
  <pre><code>pnpm test</code></pre>
</WindowMockup>
```

## ContentMask

ContentMask clips arbitrary caller-owned content to one named shape while
leaving that content's semantics, accessible name, native attributes, and
interaction intact. The neutral wrapper adds no role, accessible name, keyboard
stop, colour, radius, or Density selection. A masked image therefore keeps its
own alternative text, while a masked Button remains focusable and operable
inside the hit region established by the clip.

The complete `CONTENT_MASK_SHAPES` contract is `squircle`, `heart`, `hexagon`,
`hexagon-2`, `decagon`, `pentagon`, `diamond`, `square`, `circle`, `star`,
`star-2`, `triangle`, `triangle-2`, `triangle-3`, `triangle-4`, `half-1`, and
`half-2`. The last two expose the daisyUI half-mask modifiers as first- and
second-half clips. `CONTENT_MASK_CLIP_PATHS` publishes the corresponding
structural geometry, and `contentMask` is the optional class Extension Seam.
Page arrangement, dimensions, content, and every appearance axis remain local
to the consumer.

```svelte
<script lang="ts">
  import { Button, ContentMask } from "@reddb-io/design-system/base";
</script>

<ContentMask shape="circle" class="size-32">
  <img src={portrait} alt="Ada Lovelace" />
</ContentMask>

<ContentMask shape="diamond" class="size-32">
  <Button class="size-full">Open profile</Button>
</ContentMask>
```

## Label, Fieldset, Form, and controls

Label is the native naming primitive. Its `for` association and caller-owned
content reach a real `<label>`, so activating the label focuses or activates
the bound control. Field composes this same Label contract while adding help,
error, and required associations around one control.

Fieldset renders a real `<fieldset>` with a required visible `<legend>`. The
platform therefore keeps related controls exposed as one named group and
forwards the native disabled state to the group. Form is the native submission
boundary: action, method, validation, events, `FormData`, and imperative access
remain browser contracts. Fields, actions, business content, and page-specific
arrangement remain caller-owned.

```svelte
<Form action="/preferences" method="post">
  <Label for="display-name">Display name</Label>
  <Input id="display-name" name="displayName" />

  <Fieldset legend="Contact preferences">
    <Label><input name="notices" type="checkbox" value="email" /> Email notices</Label>
  </Fieldset>
</Form>
```

## Field, Input, Textarea, and Select

Field owns the accessible relationship around one control. It renders the
visible label, optional help and error text, a visible required marker, and
passes one typed `FieldControlProps` object to its child snippet. That object
contains the control id, native `required`, and the complete
`aria-describedby`, `aria-invalid`, and `aria-errormessage` associations. Help
and error remain associated together rather than one replacing the other.

Input is deliberately a native `<input>`, not a reduced facsimile. Identity,
type, autofill, value, native `size`, constraints, validity, events, and focus
behavior pass through. `value` is bindable, so `bind:value` works exactly as
it does on the element; checkbox, radio and file types keep their state where
the platform does (`bind:checked` on Checkbox and Switch, `bind:files` on
FileInput). A bindable `ref` is available when a consumer genuinely needs
imperative focus or selection. Field and Input are separate so the same
Field contract can wrap a consumer-owned control through the snippet seam, and
Input can stand alone when another accessible naming mechanism is appropriate.

Textarea extends the same native contract, including `bind:value`, to multiline entry. Its `rows`,
`cols`, wrapping, length constraints, validity, resize behavior, events, form
value, and focus remain the browser's. Select likewise remains a native
`<select>`: callers own its option and optgroup content, and the platform owns
the popup, arrow-key interaction, focus management, single or multiple
selection, validation, and submitted value. Neither component exposes an
external catalogue's vocabulary as public API.

```svelte
<script lang="ts">
  import { Field, Select, Textarea } from "@reddb-io/design-system/base";
</script>

<Field
  label="Deployment notes"
  help="Include the rollout and rollback plan."
  error={notesError}
  required
>
  {#snippet children(control)}
    <Textarea {...control} name="notes" rows={5} />
  {/snippet}
</Field>

<Field label="Region" required>
  {#snippet children(control)}
    <Select {...control} name="region">
      <option value="">Choose a region</option>
      <option value="sa-east-1">South America</option>
    </Select>
  {/snippet}
</Field>
```

The exported `label`, `field`, `fieldset`, `form`, `input`, `textarea`, and `select` variant functions are
the optional appearance Extension Seams. Their classes resolve through Theme,
Color Scheme, and Density roles, so the same markup renders throughout the
showcase's full appearance product without selecting an axis itself.

## SearchSelect

A closed selection control with search inside its Popover. Use it for choosing
from a long list; the existing Combobox remains the editable typeahead.
`multiple` changes the selected `value` from a string to a string array and
shows removable chips. The public types keep those two contracts distinct.

```svelte
<script lang="ts">
  import { SearchSelect, type SearchSelectOption } from "@reddb-io/design-system/base";
  const regions: readonly SearchSelectOption[] = [
    { value: "gru", label: "São Paulo", description: "Brazil", keywords: ["sa-east-1"] },
    { value: "fra", label: "Frankfurt", description: "Germany", keywords: ["eu-central-1"] },
  ];
  let region = $state("");
  let replicas = $state<string[]>([]);
</script>

<SearchSelect label="Primary region" options={regions} name="region" bind:value={region} required />
<SearchSelect multiple label="Replica regions" options={regions} name="replicas" bind:value={replicas} />
```

Search matches labels, descriptions and explicit keywords without accents or
case sensitivity; all query terms must match. Disabled choices stay visible.
Reopening starts with the complete list, and multiple selection retains earlier
choices across searches. `searchPlaceholder`, `emptyText`, `requiredMessage`
and the collapsed `placeholder` are caller-owned copy seams.

Native select controls preserve form submission, required validation, disabled
exclusion and reset even while the search popup is closed. Multiple values use
repeated form entries under the same `name`. Selection and Escape restore the
trigger's focus; Tab continues out of the picker. Clear and chip removal retain
focus on the trigger and announce the remaining selection. Field carries help
and validation relationships. `controlSize` follows Input and Button, while
Theme, Color Scheme and Density remain inherited.

## Combobox

Combobox composes the same Field association used by native inputs with Bits
UI's active-descendant listbox behavior and the canonical Popover surface
appearance. The input retains keyboard focus while arrow keys highlight
options through `aria-activedescendant`; typing narrows caller-owned option
labels without discarding the typed text. Choosing an option replaces that
text with its visible label and submits its value through the caller's form.

The component owns no business catalogue: callers pass `{ value, label,
disabled? }` options. Its visible label, help, error, required state, selected
value, current input text, and open state remain controllable. Selection is
announced in text as well as color. Collision handling comes from the anchored
overlay behavior, while `combobox`, `input`, and `popover` remain optional
appearance Extension Seams. Theme, Color Scheme, Contrast, and Density stay
outside the component API and continue to resolve from the nearest scope.

```svelte
<script lang="ts">
  import { Combobox } from "@reddb-io/design-system/base";

  const regions = [
    { value: "us-east-1", label: "North America" },
    { value: "sa-east-1", label: "South America" },
  ];
</script>

<Combobox
  label="Region"
  help="Type to narrow the available regions."
  name="region"
  options={regions}
  required
/>
```

## Checkbox, RadioGroup, and Switch

Checkbox and Switch compose the same canonical Field contract used by text
entry. Their required `label` is visible and targets a native checkbox, while
help, error, required, identity, form values, events, and imperative access
keep Field's associations and the browser's behavior. Switch fixes the native
control's role to `switch`; its moving thumb conveys checked state by position
as well as color.

RadioGroup composes the canonical Fieldset and Label contracts. A required
`legend` names the native fieldset, and one required `name` reaches every radio,
so the browser supplies exclusive selection, arrow-key behavior, validation,
and one submitted value. Callers own the option labels and values, including
whether an individual option is disabled, but cannot accidentally split the
group across different names.

```svelte
<Checkbox label="Share diagnostics" name="diagnostics" value="allowed" />

<RadioGroup
  legend="Release channel"
  name="channel"
  options={[
    { value: "stable", label: "Stable" },
    { value: "preview", label: "Preview" },
  ]}
  value="stable"
/>

<Switch label="Automatic updates" name="updates" value="automatic" />
```

The exported `checkbox`, `radioGroup`, and `switchControl` functions are their
optional token-only appearance seams. Density remains a live CSS-variable
lookup in nested scopes; none of the three controls selects Theme, Color
Scheme, or Density itself.

## Slider, Rating, ToggleButton, and ToggleGroup

Slider composes Field around a native range input. The browser retains its
numeric value, minimum, maximum, step, focus, arrow-key behavior, and submitted
form value. A visible output mirrors the current value; `formatValue` gives the
same wording to assistive technology through `aria-valuetext` when units or
domain wording matter.

Rating composes Fieldset and Label around one native radio name. Its visible
filled and empty marks make the selected count distinguishable without colour,
while the radios retain exclusive selection, focus, validation, and form
semantics. Page-specific rating meaning remains caller-owned.

ToggleButton composes the canonical Button and adds `aria-pressed`, a stable
`data-state`, and a bindable pressed value. ToggleGroup composes those buttons
inside a named Fieldset, preserves exactly one pressed option, submits the
selected value when named, and uses roving focus with arrow, Home, and End keys.
Its option labels and values belong to the consumer.

```svelte
<Slider label="Deployment capacity" name="capacity" value={40} formatValue={(value) => `${value}%`} />
<Rating legend="Release confidence" name="confidence" value={3} />
<ToggleButton label="Pin deployment" bind:pressed={pinned} />
<ToggleGroup legend="Alignment" name="alignment" options={alignments} bind:value={alignment} />
```

The exported `slider`, `rating`, `toggleButton`, and `toggleGroup` functions are
optional appearance Extension Seams. Their spatial roles remain live under
nested Density scopes, and none of the four selects Theme, Color Scheme, or
Density for its consumer.

## FileInput, OneTimeCodeInput, and ControlGroup

FileInput composes Field and Input rather than defining a second naming or
entry contract. The browser still owns file selection, accepted types,
multiple selection, validation, focus, and change events. FileInput adds the
capability-specific guarantee the native control's browser-dependent chrome
cannot make consistently: selected filenames remain visible in the document
and are announced through a polite live region. Empty single and multiple
states remain explicit as “No file selected” and “No files selected”.

OneTimeCodeInput composes Fieldset and Input into a visibly named group of
one-character native entries. Typing advances focus; Backspace on an empty
segment, arrows, Home, and End move predictably; and a pasted alphanumeric code
is distributed from the active segment instead of being discarded. One hidden
native input carries the complete value into FormData, while every visible
segment retains its own required, invalid, help, and error associations.

ControlGroup owns only related-control arrangement. Its required label names a
native `role="group"`; its horizontal or vertical orientation never changes
the order or focus behavior of caller-owned children. Page-specific actions,
business labels, and compositions remain local to the consumer.

```svelte
<FileInput label="Upload evidence" name="evidence" accept=".pdf,image/*" />

<OneTimeCodeInput
  label="Verification code"
  name="verificationCode"
  length={6}
/>

<ControlGroup label="Record actions">
  <Button type="submit">Save</Button>
  <Button type="button" variant="secondary">Cancel</Button>
</ControlGroup>
```

The exported `fileInput`, `oneTimeCodeInput`, and `controlGroup` functions are
their optional appearance Extension Seams. Every spatial value resolves from
the active Density scope, and none of the components select Theme, Color
Scheme, or Density for the consumer.

## DateField and DateRangeField

DateField composes the canonical Field and Input contracts into year, month,
and day segments. Values use complete ISO `YYYY-MM-DD` form; impossible or
incomplete calendar dates never reach the named hidden form control. Complete
segment entry advances focus, and Left and Right arrows move predictably
between segments. The caller continues to own the visible label, help, error,
requirement, and form name while every segment inherits its Field associations.

DateRangeField composes two DateFields inside the canonical native Fieldset.
Its caller owns the group legend, both bound labels, help text, and inverted
range message. An end before its start is announced on the group and is not
published under the end form name; correcting the bound restores the normal
ordered pair. Calendar selection, page arrangement, and scheduling policy stay
outside this entry contract.

```svelte
<DateField label="Deployment date" name="deploymentDate" value="2026-08-20" required />

<DateRangeField
  label="Deployment window"
  startLabel="Starts"
  endLabel="Ends"
  startName="deploymentStart"
  endName="deploymentEnd"
  startValue="2026-08-10"
  endValue="2026-08-20"
/>
```

The exported `dateField` and `dateRangeField` functions are optional appearance
Extension Seams. They compose Field, Input, and Fieldset token roles, remain
Density-responsive, and select no appearance axis themselves.

## DatePicker and DateRangePicker

DatePicker composes the canonical DateField, Calendar, and Popover contracts
without creating a second date model. Typing a complete possible ISO date
selects the same day in the calendar; choosing a calendar day updates the
segmented field and its named form value before the Popover closes. The caller
owns labels, validation copy, availability rules, form naming, and initial
value.

DateRangePicker applies the same composition to DateRangeField and
RangeCalendar. A first calendar click publishes only a start and leaves the
Popover open; the complete ordered range closes it and publishes both bound
form values. The RangeCalendar continues to own roving focus and range
announcements, while Popover continues to own collision handling, Escape, and
focus return.

```svelte
<DatePicker
  label="Deployment date"
  calendarLabel="Deployment date calendar"
  name="deploymentDate"
  value="2026-08-20"
/>

<DateRangePicker
  label="Deployment window"
  startLabel="Starts"
  endLabel="Ends"
  calendarLabel="Deployment window calendar"
  startName="deploymentStart"
  endName="deploymentEnd"
  startValue="2026-08-20"
  endValue="2026-08-24"
/>
```

The optional `datePicker` and `dateRangePicker` appearance seams own only the
relationship between the composed contracts. Their geometry resolves through
Density tokens, and neither picker selects Theme, Color Scheme, or Density.

## TimeField and TimeRangeField

TimeField composes the canonical Field and Input contracts into hour and
minute segments. Values use complete 24-hour `HH:mm` form; impossible or
incomplete values never reach the named hidden form control. Two-digit entry
advances focus, and Left and Right arrows move predictably between segments.
The caller continues to own the visible label, help, error, requirement, and
form name while both segments inherit their Field associations.

TimeRangeField composes two TimeFields inside the canonical native Fieldset.
Its caller owns the group legend, both bound labels, help text, and inverted
range message. An end before its start is announced on the group and is not
published under the end form name; correcting the bound restores the normal
ordered pair. The component owns no scheduling policy or page arrangement.

```svelte
<TimeField label="Meeting time" name="meetingTime" value="09:45" required />

<TimeRangeField
  label="Support window"
  startLabel="Starts"
  endLabel="Ends"
  startName="supportStart"
  endName="supportEnd"
  startValue="09:00"
  endValue="17:00"
/>
```

The exported `timeField` and `timeRangeField` functions are optional
appearance Extension Seams. They compose Field, Input, and Fieldset token
roles, remain Density-responsive, and select no appearance axis themselves.

## Disclosure family

`Tabs`, `Accordion`, and `Disclosure` compose the canonical Base `Button`
with Bits UI's focus and expanded-state contracts. They own only the reusable
interaction: the consumer supplies stable item values, visible labels, and
snippet content, while page arrangement and business language remain local.

```svelte
<Tabs label="Deployment view" items={tabs} bind:value>
  {#snippet children({ value })}
    <DeploymentPanel panel={value} />
  {/snippet}
</Tabs>

<Accordion label="Release questions" items={questions} bind:expanded>
  {#snippet children({ value })}
    <Answer question={value} />
  {/snippet}
</Accordion>

<Disclosure label="Advanced details">
  <DeploymentDetails />
</Disclosure>
```

Tabs use automatic activation with roving focus: horizontal arrows, Home, and
End move both focus and selection, skipping disabled tabs. Accordion triggers
publish `aria-expanded` and their controlled region relationship; single mode
is the default and `multiple` opts into independent sections. Disclosure is
the one-section form of the same contract. The exported `tabs`, `accordion`,
and `disclosure` functions are the optional token-only appearance seams. None
of the three selects Theme, Color Scheme, or Density.
With `followHash`, Tabs also selects the panel containing a same-page fragment,
including nested tab sets, then focuses the exact linked target. Product links
remain ordinary anchors rather than page-local tab-control scripts.

## Avatar, MediaObject, and Carousel

Avatar is a named image boundary with a required visible text fallback. The
image is optional; when it is absent or fails, the fallback is already in the
same box. `sm`, `md`, and `lg` name Density-responsive control-height roles,
not fixed dimensions. The component supplies no Theme, Color Scheme, or
Density scope of its own.

MediaObject keeps caller-owned leading media and content in one generic
relationship. Its `media` snippet commonly composes Avatar, while headings,
metadata, actions, business language, and page arrangement remain local to the
consumer. Alignment and gap are independent, and every gap names a live
Density role.

Carousel is a labeled region of caller-owned slides. Previous and next are the
canonical Base Button, each slide exposes its position and label, and inactive
slides leave both the visual and keyboard flow. Automatic rotation is opt-in;
when enabled it always adds a pause/resume Button, uses a quiet live region
while moving, and stops when focus or hover enters caller content.

```svelte
<script lang="ts">
  import { Avatar, MediaObject } from "@reddb-io/design-system/base";
</script>

{#snippet portrait()}
  <Avatar name="Ada Lovelace" fallback="AL" />
{/snippet}

<MediaObject media={portrait}>
  <strong>Ada Lovelace</strong>
  <p>Caller-owned biography and actions.</p>
</MediaObject>
```

## Table

Table turns caller-owned column labels and row values into one native table.
Its visible caption names both the table and its responsive scroll region;
every column heading carries `scope="col"`, and the `rowHeader` column carries
`scope="row"`. The first column is the row header by default. Wide content
scrolls horizontally inside a focusable region, so overflow is reachable from
the keyboard instead of being clipped or silently extending the page.

The optional `cell` snippet keeps links, controls, formatting, and business
content local to the consumer while Table retains the native header
relationships. All spacing names Density roles and the component declares no
Theme, Color Scheme, or Density of its own.

```svelte
<script lang="ts">
  import { Table } from "@reddb-io/design-system/base";

  const columns = [
    { key: "node", header: "Node" },
    { key: "region", header: "Region" },
    { key: "latency", header: "Latency" },
  ];
  const rows = [
    { node: "db-01", region: "South America", latency: "24 ms" },
  ];
</script>

<Table caption="Cluster replicas" {columns} {rows} rowHeader="node" />
```

## Statistic, Countdown, and Timeline

Statistic composes the canonical DescriptionList term/detail relationship: its required label is
the term, and its `<data>` detail keeps the caller's scalar machine-readable even when
`formatValue` supplies a localized visible rendering. Optional supporting context remains caller
content. Statistic owns no dashboard grid, comparison period, trend meaning, or business metric.

Countdown is controlled through `remaining` whole seconds, so scheduling and the source of time
stay with the consumer. While running it uses the quiet timer role and a native `<time>` duration;
at zero it becomes one polite, atomic status containing the visible label and completion sentence.
Negative and non-finite inputs normalize to completion. `formatDuration` and
`normalizeRemaining` are exported as the shared behavior seam.

Timeline owns native ordered-list chronology and native machine-readable event times. Event titles
with destinations compose the canonical Link, while plain events add no keyboard stop. The caller
owns event wording, timestamps, destinations, and page arrangement.

```svelte
<script lang="ts">
  import { Countdown, Statistic, Timeline } from "@reddb-io/design-system/base";
</script>

<Statistic label="Queries" value={12840} description="Last 24 hours" />
<Countdown label="Deployment window" remaining={65} />
<Timeline
  label="Deployment history"
  items={[
    { id: "queued", title: "Queued", datetime: "2026-08-08T19:00:00Z", time: "19:00" },
  ]}
/>
```

## Progress, RadialProgress, Meter, LoadingIndicator, and Skeleton

Progress and RadialProgress share one exported range-normalization contract.
A finite value produces matching visual length, `aria-valuenow`, and value
text; omitting it produces an indeterminate progressbar with no invented
current value. RadialProgress adds a visible number to its arc, so current
state is never carried by color alone. `Progress` can opt into a visible
`summary` that repeats its accessible label and normalized value beside the
track without creating a second accessibility owner.

Meter represents a bounded scalar rather than work in flight. It exposes the
meter role and normalized range while drawing both a textual value and a fill
length. Its label and value language remain caller-owned.

LoadingIndicator is the canonical standalone waiting status. Its sentence is
always in the accessibility tree, its spinner is decorative, and the region
announces a polite busy state. Rotation is gated by the platform motion
preference. The Application Kit's former `LoadingState` duplicate has been
retired; every consumer receives this Base contract instead.

Skeleton offers text, rectangle, and circle placeholders while leaving their
arrangement and final dimensions to the consumer. It is decorative by default;
a caller may give it a label when the placeholder itself owns the polite busy
announcement. Its pulse is motion-safe and its rectangle height remains a live
Density role.

```svelte
<script lang="ts">
  import {
    LoadingIndicator,
    Meter,
    Progress,
    RadialProgress,
    Skeleton,
  } from "@reddb-io/design-system/base";
</script>

<Progress value={72} label="Deployment progress" summary />
<RadialProgress value={64} label="Indexing progress" />
<Meter value={3} max={5} label="Cluster capacity" />
<LoadingIndicator label="Loading nodes…" />
<Skeleton shape="rectangle" label="Loading node summary" />
```

## The Logo

The Brand owns the Mark; the DS owns the component that places it (ADR 0004).
So the Logo does four things and improvises at none of them.

**It selects.** `layout` is `horizontal`, `stacked` or `icon`; `on` is `light`
or `dark` and names the **surface behind the Mark**, not the Theme — a dark
hero on a light marketing page is `on="dark"`. Unset, `on` defaults from the
active Color Scheme, read off `data-color-scheme`. It reacts when that root
attribute changes, so persistent navigation swaps between the approved color
and inverse Marks without coupling contrast to the directional Theme. With no
document — a server render — it reads as `light`. A local surface that differs
from the document still says so explicitly with `on`.

**It refuses.** When the pinned Assets release ships no Mark for the pair asked
for — today the icon on dark — the Logo throws, naming the gap and the release.
It never falls back to the other surface's drawing and never recolors one:
both are licence violations, so neither has a code path. The message is the
first draft of the Brand proposal that fixes it.

**It holds the clearspace.** 25% of the symbol's height, inside the component's
own box, with no opt-out prop. `size` is the symbol's height in CSS pixels —
the symbol is the same drawing in every layout, so one number means the same
thing across all three — and the rendered box is larger by the clearspace on
every side. Below `MINIMUM_SYMBOL_PX` is an error, not a smaller Logo.

**It links, when asked.** With an `href` it becomes a real `<a>` carrying an
`aria-label`, and the focus ring is drawn on the box — which already includes
the clearspace, so the ring falls outside it by construction. Without one it is
the pure Mark: a footer, a chatbot avatar.

When `on` is omitted, the Logo observes the root Color Scheme attribute. An
explicit `on` remains static and needs no observation.

### The Marks

`src/marks/*.svg` are the pinned release's bytes, vendored **inside** `src`
because that is the whole of what a consumer receives. They are copies of
`vendor/brand/marks/`, and `test/marks.test.ts` re-hashes them against the same
`brand.lock.json` digests `scripts/producer` checks — one pinned release, two
places it has to hold, never two pins. The Logo reaches a Mark by importing the
file, so the drawing stays opaque bytes the whole way through and "reproduced
unaltered" is a property of how the component works rather than a rule someone
has to follow.

## The anti-hardcode lint

Every colour and radius a Kit wears must name a token the Themes reassign, and
every spatial value the Density axis ships a role for must name that role. The
rule arrived here the way this README said it would: by **extracting** the
application Kit's linter into `@reddb-io/kit-lint`, which both Kits now run.
What is this Kit's own is `tools/lint-cli.ts` and the files it hands over —
which are its `.svelte` and `.ts` source, and never a Mark, whose colours are
the Brand's drawing rather than a Kit hardcoding one.

```
pnpm --filter kit-base lint
```

## Consuming it

For native React, see the [React implementation](react/README.md). Its initial
coverage is twelve components; Svelte remains the default Sync framework.

Same as any Kit: `ds-sync` lands it as vendorable Svelte 5 source at a pinned
DS release (ADR 0002), inside the one package it assembles
(`scripts/producer/README.md`), where this Kit is the `./base` subpath:

```ts
import {
  Accordion,
  Alert,
  AlertDialog,
  Button,
  ControlGroup,
  DateField,
  DateRangeField,
  Dialog,
  Disclosure,
  Drawer,
  Field,
  Fieldset,
  FileInput,
  Form,
  Input,
  Label,
  Logo,
  OneTimeCodeInput,
  Select,
  Tabs,
  Textarea,
  TimeField,
  TimeRangeField,
  Tooltip,
  alert,
  accordion,
  alertDialog,
  alertDialogActions,
  button,
  controlGroup,
  dateField,
  dateRangeField,
  dialog,
  disclosure,
  drawer,
  field,
  fieldset,
  fileInput,
  form,
  input,
  label,
  oneTimeCodeInput,
  select,
  tabs,
  textarea,
  timeField,
  timeRangeField,
  tooltip,
  type AlertFeedbackRole,
  type ButtonVariant,
  type ControlGroupOrientation,
  type FieldControlProps,
} from "@reddb-io/design-system/base";
```

The difference is only that no consumer has to ask for it.

## Commands

```
pnpm --filter kit-base test    # public behavior, accessibility, routing, Marks, and consumer checks
pnpm --filter kit-base check   # consumer svelte-check over the Kit's source, strict tsconfig
pnpm --filter kit-base lint    # every colour and radius names a token the Themes reassign
pnpm --filter kit-base build   # stage the vendorable source into dist/ for the release bundle
```

### The consumer check

A Kit ships as source, so the compiler that judges it is the consumer's, and
`tsconfig.consumer.json` is that compiler with no DS config behind it. It adds
one thing the application Kit's does not — `types: ["vite/client"]` — and that
is the point: the Logo imports its Mark, and a consumer's own bundler is what
declares `*.svg`. This Kit ships no such declaration (`types/marks.d.ts` stays
outside `src/`), because a wildcard ambient module can only be declared once in
a program and a second copy would fail a consumer's build in a file they never
wrote. `test/consumer-check.test.ts` runs the check on every `test`, so that
cannot regress unnoticed.

## Minimum configuration

Button defaults to secondary emphasis; the leading action selects `variant="primary"`. Stack spaces blocks with `layout-md` by default; use a component gap for small internal groups and `gap="none"` for contiguous content. Card owns the layout-sm rhythm between body blocks. These defaults inherit the selected Theme, Color Scheme and Density. Controls consume pointer-aware editable text, so touch entry is at least 16px without per-component touch overrides. See ADR 0029 and the first specimens in the showcase.

### Inline measurements

`Meter layout="inline" labelHidden` places its track beside the reading for table cells. The label remains its accessible name; the caller supplies the metric and threshold tone. The default stacked layout is unchanged.

`ScrollArea variant="plain"` embeds a named, keyboard-scrollable region in an existing panel without adding another frame or inset. The default `outline` remains appropriate for standalone regions.

### Inline editing

`InlineEdit` composes canonical Input and Button around a visible field label.
`bind:value` is the committed text; keystrokes stay in an independent draft until
Save or Enter succeeds. Escape or Cancel discards the draft and returns focus to
the edit action. An externally updated value does not overwrite an open draft.

```svelte
<InlineEdit label="Project name" bind:value={name}
  validate={value => value.trim().length < 3 ? "Use at least three characters." : undefined}
  onconfirm={value => persistName(value)} />
```

The application owns validation and persistence. A rejected `onconfirm` retains the
draft with an announced `saveErrorMessage`, and a pending save prevents duplicate
submission. `onvaluechange` follows successful persistence. `editing` is bindable;
its paired callback is `oneditingchange`. Data fetching and router interception
remain outside this component. The native form and input need no popup focus
manager. See `/kit/inline-edit` for validation, retry and disabled examples.

## Carousel rendering seam

`viewport` is an optional `Snippet<[CarouselViewportContext]>` that replaces only
slide drawing. It receives `slides`, the clamped `active` index, `playing` and
`firstIndex`. Base retains previous/next controls, position, rotation and pausing
on focus or pointer interaction. The custom renderer must provide named slide
groups, exclude inactive content from focus and accessibility, honor Image
priority and carry its own reduced-motion presentation. Marketing's
`PerspectiveCarousel` is the canonical example of this seam.

## Operational presentation

`Progress` and `Meter` accept optional `segments` and `segmentColumns`. Divisions
are decorative and capped at 128 rendered cells; exact range/value semantics and
fractional fill remain intact. Omit segments for the continuous default.
Indeterminate Progress retains its normal representation.

`Statistic animate` gives paint-only update feedback; it does not interpolate the
semantic value or announce intermediate numbers. `Transfer` takes `fromId`,
`toId` and a changing `trigger` after the application has committed state. Both
anchors remain application-owned; the decorative clone is inert, interruptible
and removed under reduced motion, navigation, cancellation or unmount.

### Saved values and explicit editing

Use `InlineEdit` with `presentation="input"` for a committed value loaded from
an API, file or other application-owned source. The read view is selectable HTML text, without a textbox, border or input focus stop.
Only activating Edit mounts the native input. `sourceLabel` describes
provenance; `savedLabel` defaults to `Saved`. Neither infers network state.

`editVisibility="hover"` is optional: focus also reveals Edit, and touch always
shows it. Save validates and awaits `onconfirm` (Svelte) or `onConfirm` (React)
before committing. A rejected save retains the draft and announces the error;
Escape or Cancel restores the committed value and returns focus to Edit.
Pass an optional `icon` glyph (for example Phone) and `inputType="tel"` for a saved
telephone number. The glyph stays decorative in both states; passwords remain
masked in the read view.
InlineEdit owns its edit form; do not nest it inside another form.

```svelte
<InlineEdit label="Project name" presentation="input" sourceLabel="Configuration API"
  bind:value={projectName} onconfirm={saveProjectName} />
```

```tsx
<InlineEdit label="Project name" presentation="input" sourceLabel="Configuration API"
  value={projectName} onValueChange={setProjectName} onConfirm={saveProjectName} />
```

### Renderer-free extension seams

Import visual recipes from `@reddb-io/design-system/base/recipes`, never from a
component barrel when implementing portable styling. This seam has no React,
Svelte, behavior adapter or chart-engine dependency. Each other Kit exposes its
own `/recipes` seam as well; Sync preserves it in source and compiled delivery.

A host shell that handles Escape must yield events from a modal:

```ts
import { isEventFromModal } from "@reddb-io/design-system/base/interaction";
function onKeyDown(event: KeyboardEvent) {
  if (event.defaultPrevented || isEventFromModal(event)) return;
  if (event.key === "Escape") closeHost();
}
```

Pass `event.nativeEvent` from a React event handler. This permits managed popups
to consume Escape before the Dialog, and the Dialog before the host. The modal
retains native focus containment and restores its trigger on close.

## International phone entry

`PhoneInput` uses country metadata to format national numbers while editing.
The country picker shows flags and calling codes, and searches localized names,
English names, ISO codes and DDIs. `locale` controls country names; it never
infers location. Set `defaultCountry` explicitly (the fallback is `US`) and
optionally restrict `countries`. International paste infers the actual country;
a pasted number outside the allowlist is retained but blocks submission.

`value`, `defaultValue`, `onvaluechange` (Svelte), `onValueChange` (React) and
the single named `FormData` entry use an **unmasked international string**,
for example `+5511999991234`. The plus sign is retained; spaces, parentheses,
hyphens and national trunk prefixes are removed. Draft values can be incomplete;
native form validation blocks incomplete/unsupported numbers, and `required`
blocks empty values. Length/structure checks do not verify number ownership.
`help`, `error`, `disabled`, `form`, reset and `controlSize` retain native field
semantics. Disabled values are omitted.

Use `presentation="text"` and optional `sourceLabel` for selectable saved HTML
text. The application owns Edit/Save/Cancel and persistence, switching to
`presentation="input"` for a draft. See `/kit/phone-input` and
`/examples/phone-forms` for both flavors and actual form payloads.

Import `PhoneInput` from the canonical Base Kit. Its pinned
`libphonenumber-js/min` engine and metadata are component-scoped; unrelated
components retain their tree-shaking boundary. Flags need no image bundle or
external requests. Sync selects `framework: "react"` or `"svelte"`.

## Protected values

`SecretValue` presents a named sensitive value as selectable text, masked by
default with a fixed-length visual marker. Set `revealable` only after the
application has authorized the action. `onrevealedchange(true, "control")`
requests retrieval; supply `value` and `revealed` only after that succeeds.
Loading and caller-safe errors remain application-owned.

The shared exposure controller expires after `hideAfter` milliseconds (30000 by
default; zero explicitly disables expiry), without extending the deadline on
ordinary refreshes. Hiding, permission loss, replacement values and hidden
documents remove plaintext from the rendered DOM. Handle false callbacks by
clearing the supplied value as well. The component stores no credentials and
provides no access control; masking does not make client-held plaintext secret.
See `/kit/secret-value` and `/examples/operations-workbench` for both flavors.
