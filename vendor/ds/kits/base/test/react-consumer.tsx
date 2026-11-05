// Consumer-facing type contracts. These never enter the vendored source closure.
import { createRef } from "react";
import { ContentMask, Eyebrow, Fieldset, Form, Prose, GridList, Indicator, List, Meter, SectionHeading } from "../react/src";
const ul = createRef<HTMLUListElement>();
const ol = createRef<HTMLOListElement>();
const div = createRef<HTMLDivElement>();
const form = createRef<HTMLFormElement>();
const article = createRef<HTMLElement>();
const span = createRef<HTMLSpanElement>();
<Prose as="article" ref={article} />;
<Eyebrow as="span" ref={span}>Overview</Eyebrow>;
<List ref={ul}><li>One</li></List>;
<List ordered ref={ol} start={3}><li>Three</li></List>;
<GridList list={false} ref={div}><div>Layout tile</div></GridList>;
<GridList ref={ul}><li>List tile</li></GridList>;
<Form ref={form} action={data => { data.get("email"); }} />;
<Fieldset legend="Recipients">{control => <input {...control} aria-label="Recipient" />}</Fieldset>;
<Indicator marker={<span>3</span>}><button>Inbox</button></Indicator>;
<SectionHeading title="Requests" actions={<button>Details</button>} />;
<ContentMask shape="hexagon"><span>Content</span></ContentMask>;
<Meter value={30} tone="success" />;
// @ts-expect-error Ordered lists expose the actual ol native ref.
<List ordered ref={ul} />;
// @ts-expect-error A layout grid exposes its div, not a list ref.
<GridList list={false} ref={ul} />;
// @ts-expect-error Native group naming is required.
<Fieldset />;
// @ts-expect-error A marker's anchored content is required.
<Indicator marker="3" />;
// @ts-expect-error A measured value is required.
<Meter />;
// @ts-expect-error New React APIs do not inherit deprecated Svelte aliases.
<Meter value={30} tone="primary" />;

// @ts-expect-error A div's native ref is required when the element is div.
<Eyebrow as="div" ref={span}>Overview</Eyebrow>;

import { Card, Icon, Image, Logo } from "../react/src";
import { Camera } from "lucide-react";
const anchor = createRef<HTMLAnchorElement>();
const svg = createRef<SVGSVGElement>();
const img = createRef<HTMLImageElement>();
<Icon icon={Camera} ref={svg} color="ink-muted" />;
<Icon icon="menu" ref={svg} />;
<Image src="/photo.webp" alt="Region" width={800} height={600} ref={img} sources={[{ srcset: "/photo.avif", type: "image/avif" }]} />;
<Logo href="/" ref={anchor} label="RedDB home" />;
<Logo ref={span} on="dark" />;
<Card ref={div} icon={Camera} image={{ src: "/photo.webp", alt: "Region", ratio: 1 }} footer={<button>Open</button>}>Body</Card>;
// @ts-expect-error Semantic ink roles exclude arbitrary colors.
<Icon icon={Camera} color="blue" />;
// @ts-expect-error Glyph is required.
<Icon />;
// @ts-expect-error Image alternatives are required.
<Image src="/photo.webp" ratio={1} />;
// @ts-expect-error Mark mode does not accept a link label.
<Logo label="Not a link" />;
// @ts-expect-error A linked Logo requires an anchor ref.
<Logo href="/" ref={span} />;
// @ts-expect-error Cards only accept native React glyphs.
<Card icon="menu" />;

import { Alert, Button, Breadcrumbs, BrowserMockup, EmptyState, Link, ListContainer, MediaObject, Notification, PhoneMockup, WindowMockup } from "../react/src";
import type { BreadcrumbItem } from "../react/src";
const trail: BreadcrumbItem[] = [{ id: "home", label: "Home", href: "/" }];
<Link href="/" ref={anchor}>Home</Link>;
<Alert tone="danger" ref={div}>An error occurred.</Alert>;
<Notification title="Saved" onDismiss={() => {}} ref={div} />;
<EmptyState title="No requests" actions={<Button>New request</Button>} ref={div} />;
<MediaObject media={<Icon icon="menu" />} ref={div}>Content</MediaObject>;
<ListContainer title="Destinations" header={<strong>Header</strong>} ref={div}><List><li>One</li></List></ListContainer>;
<Breadcrumbs items={trail} structuredData={{ origin: "https://example.com" }} />;
<BrowserMockup address="example.com" ref={div}><Link href="/">Live content</Link></BrowserMockup>;
<WindowMockup title="Window" ref={div}><span>Content</span></WindowMockup>;
<PhoneMockup ref={div}><span>Content</span></PhoneMockup>;
// @ts-expect-error Links always require destinations.
<Link>Home</Link>;
// @ts-expect-error Empty states always name the missing content.
<EmptyState />;
// @ts-expect-error Notifications require announcement text.
<Notification />;
// @ts-expect-error React uses the canonical tone name.
<Alert feedback="danger" />;
// @ts-expect-error Both regions of a MediaObject are required.
<MediaObject>Content</MediaObject>;
// @ts-expect-error Decorative chrome cannot hide caller content.
<BrowserMockup aria-hidden><span>Content</span></BrowserMockup>;
// @ts-expect-error Decorative frames always require caller content.
<PhoneMockup />;

import { Countdown, Pagination, ScrollArea, Steps, Table, Timeline } from "../react/src";
import type { TableCellContext, TableColumn, TableRow, TimelineItem, StepItem, PaginationPage } from "../react/src";
const tableRef = createRef<HTMLTableElement>();
const columns: TableColumn[] = [{ key: "name", header: "Name" }];
const rows: TableRow[] = [{ name: "Primary" }];
const stepItems: StepItem[] = [{ id: "one", label: "Set up", state: "current" }];
const timelineItems: TimelineItem[] = [{ id: "one", title: "Created", datetime: "2026-10-19", time: "Today" }];
const pageItems: PaginationPage[] = [{ page: 1, href: "/?page=1", current: true }];
<Table caption="Clusters" columns={columns} rows={rows} ref={tableRef} cell={({ value, rowIndex }: TableCellContext) => <Link href={"/cluster/" + rowIndex}>{value}</Link>} />;
<Timeline items={timelineItems} ref={ol} start={3} />;
<Steps items={stepItems} />;
<Pagination pages={pageItems} />;
<ScrollArea label="Activity" ref={div}>Scrollable content</ScrollArea>;
<Countdown remaining={65} ref={div} formatValue={value => value + " seconds"} />;
// @ts-expect-error Tables require a visible caption.
<Table columns={columns} rows={rows} />;
// @ts-expect-error Native Table ref points to the table, not the outer region.
<Table caption="Clusters" columns={columns} rows={rows} ref={div} />;
// @ts-expect-error Timeline data includes a machine-readable time.
<Timeline items={[{ id: "x", title: "Event", time: "Today" }]} />;
// @ts-expect-error Step state follows the canonical closed vocabulary.
<Steps items={[{ id: "x", label: "Event", state: "active" }]} />;
// @ts-expect-error Page links require an href.
<Pagination pages={[{ page: 1 }]} />;
// @ts-expect-error A keyboard-scrollable region requires a name.
<ScrollArea />;
// @ts-expect-error Remaining duration is caller-owned and required.
<Countdown />;

import { FileInput, Rating, ToggleButton } from "../react/src";
const fileRef = createRef<HTMLInputElement>();
const ratingRef = createRef<HTMLFieldSetElement>();
const toggleRef = createRef<HTMLButtonElement>();
<FileInput label="Evidence" ref={fileRef} files={null} onFilesChange={files => files?.item(0)} multiple />;
<Rating legend="Confidence" name="confidence" ref={ratingRef} defaultValue={3} onValueChange={value => value.toFixed()} />;
<ToggleButton label="Pin review" ref={toggleRef} defaultPressed onPressedChange={pressed => pressed.valueOf()} />;
// @ts-expect-error Native file selection cannot be supplied as a string value.
<FileInput label="Evidence" value="/tmp/report.txt" />;
// @ts-expect-error Native file inputs cannot be prepopulated by a string default.
<FileInput label="Evidence" defaultValue="report.txt" />;
// @ts-expect-error A rating requires a serialization name.
<Rating legend="Confidence" />;
// @ts-expect-error The component owns aria-pressed and cannot submit a form.
<ToggleButton label="Pin" type="submit" />;
// @ts-expect-error A toggle forwards its actual button ref.
<ToggleButton label="Pin" ref={anchor} />;

import { Checkbox, Switch, RadioGroup, Select } from "../react/src";
const choiceRef = createRef<HTMLInputElement>();
const selectRef = createRef<HTMLSelectElement>();
<Checkbox label="Confirm" name="confirm" required ref={choiceRef} defaultChecked onCheckedChange={checked => checked.valueOf()} />;
<Switch label="Digest" checked={true} ref={choiceRef} onCheckedChange={checked => checked.valueOf()} />;
<RadioGroup legend="Policy" name="policy" ref={ratingRef} options={[{ value: "a", label: "Balanced", description: "Details" }]} variant="cards" optionMedia={option => <span>{option.value}</span>} defaultValue="a" />;
<Select aria-label="Region" ref={selectRef} multiple defaultValue={["sa"]} options={[{ value: "sa", label: "South America" }]} onChange={event => event.currentTarget.selectedOptions} onValueChange={value => value.toUpperCase()} />;
// @ts-expect-error A native input ref is required for a checkbox.
<Checkbox label="Confirm" ref={selectRef} />;
// @ts-expect-error A Switch owns its native role.
<Switch label="Digest" role="radio" />;
// @ts-expect-error Radio groups require caller-owned choices.
<RadioGroup legend="Policy" name="policy" />;
// @ts-expect-error Semantic callback emits a native value, not a change event.
<Select onValueChange={event => event.currentTarget.value} />;
// @ts-expect-error Radio option media is a native render function.
<RadioGroup legend="Policy" name="policy" options={[]} optionMedia={<span>Media</span>} />;

import { SlideFrame, TitleSlide, SectionSlide, TwoColumnSlide, FigureSlide, QuoteSlide, BulletsSlide, Deck } from "../react/src";
<Deck label="Review" role="region" ref={article}><SlideFrame label="Overview" title="Native tooltip" ref={div} number={1} total={7}><button>Read</button></SlideFrame></Deck>;
<TitleSlide title="Review" level={3} surface="raised" ref={div} />;
<SectionSlide title="Coverage" summary="The next rollout" ref={div} />;
<TwoColumnSlide title="Options" start={<p>Retain</p>} end={<p>Verify</p>} ref={div} />;
<FigureSlide title="Readiness" figure={<table />} caption="Snapshot" ref={div} />;
<QuoteSlide quote="Ready for review" attribution="Operations" source="Review" ref={div} />;
<BulletsSlide title="Actions" items={["Review", "Ship"]} ref={div} />;
// @ts-expect-error Slides require an accessible name.
<SlideFrame />;
// @ts-expect-error Decks require a name.
<Deck />;
// @ts-expect-error Both caller-owned columns are required.
<TwoColumnSlide title="Options" start={<p>Retain</p>} />;
// @ts-expect-error Figures use native React content rather than a Svelte snippet.
<FigureSlide title="Readiness" figure={() => <table />} />;
// @ts-expect-error The frame owns the slide role.
<SlideFrame label="Overview" role="button" />;
// @ts-expect-error Slides expose a native div ref.
<TitleSlide title="Review" ref={span} />;
// @ts-expect-error Outline levels retain the canonical closed vocabulary.
<TitleSlide title="Review" level={7} />;
// @ts-expect-error Surface roles retain the canonical closed vocabulary.
<SlideFrame label="Overview" surface="brand" />;

import { NumberField, Slider, OneTimeCodeInput } from "../react/src";
const nativeInput = createRef<HTMLInputElement>(), codeGroup = createRef<HTMLFieldSetElement>();
<NumberField label="Capacity" defaultValue={3} min={1} max={12} step={0.1} ref={nativeInput} onValueChange={next => { const value: number | null = next; }} />;
<NumberField label="Capacity" value={null} readOnly />;
<Slider label="Sampling" defaultValue={35} step="any" ref={nativeInput} formatValue={next => `${next}%`} onValueChange={next => { const value: number = next; }} />;
<OneTimeCodeInput label="Code" defaultValue="012345" inputMode="numeric" pattern="[0-9]" ref={codeGroup} onComplete={next => { const value: string = next; }} />;
// @ts-expect-error Native numeric controls cannot receive string values.
<NumberField label="Capacity" value="3" />;
// @ts-expect-error Null is reserved for empty numeric text fields, not ranges.
<Slider label="Sampling" value={null} />;
// @ts-expect-error Native OTP keyboard hints retain the shared closed vocabulary.
<OneTimeCodeInput label="Code" inputMode="password" />;
// @ts-expect-error The native segmented group exposes a fieldset ref.
<OneTimeCodeInput label="Code" ref={nativeInput} />;
// @ts-expect-error Numeric controls expose their input, not their Field root.
<NumberField label="Capacity" ref={div} />;
// @ts-expect-error Code entry retains a required visible name.
<OneTimeCodeInput />;

import { Swap, CodeBlock, type CodeTokenizer } from "../react/src";
const tokenizer: CodeTokenizer = { tokenize: code => [{ kind: "text", value: code }] };
<Swap label="Pin request" defaultSwapped off={<span>Pin</span>} on={<strong>Pin</strong>} ref={toggleRef} onClick={event => event.currentTarget.focus()} onSwappedChange={next => next.valueOf()} />;
<CodeBlock code="select 1;" grammar={tokenizer} ref={div} copyErrorLabel="Retry copy" />;
// @ts-expect-error Swap requires a stable accessible name.
<Swap />;
// @ts-expect-error A swap never submits a form.
<Swap label="Pin" type="submit" />;
// @ts-expect-error Deprecated Svelte alias is not a new React API.
<Swap label="Pin" onChange={() => {}} />;
// @ts-expect-error Face content is native React content, not a snippet function.
<Swap label="Pin" on={() => <span>Pin</span>} />;
// @ts-expect-error CodeBlock requires exact source text.
<CodeBlock />;
// @ts-expect-error The source cannot be replaced by caller-owned children.
<CodeBlock code="select 1;"><p>Different source</p></CodeBlock>;
// @ts-expect-error CodeBlock exposes its native root div ref.
<CodeBlock code="select 1;" ref={toggleRef} />;

import { InlineEdit, Reveal, type RevealDirection } from "../react/src";
const entryDirection: RevealDirection = "up";
<InlineEdit label="Project" value="North" defaultEditing maxLength={40} ref={div} onConfirm={async value => { value.toUpperCase(); }} onValueChange={value => value.toUpperCase()} onEditingChange={next => next.valueOf()} />;
<Reveal direction={entryDirection} delay={120} debug ref={div}><p>Review</p></Reveal>;
// @ts-expect-error Committed text remains required.
<InlineEdit label="Project" />;
// @ts-expect-error Inline editing retains a required visible name.
<InlineEdit value="North" />;
// @ts-expect-error React retains native maxLength casing.
<InlineEdit label="Project" value="North" maxlength={40} />;
// @ts-expect-error Persistence resolves void; booleans cannot silently mean confirmation.
<InlineEdit label="Project" value="North" onConfirm={async () => false} />;
// @ts-expect-error The field-group ref is a div.
<InlineEdit label="Project" value="North" ref={toggleRef} />;
// @ts-expect-error Direction retains the canonical closed vocabulary.
<Reveal direction="diagonal" />;
// @ts-expect-error Motion delay is a number of milliseconds.
<Reveal delay="slow" />;


// Identity alternatives, native groups, and a caller-owned appearance axis.
import { Avatar, ToggleGroup, AppearanceSwitch } from "../react/src";
const avatarRef = createRef<HTMLSpanElement>();
const scopeRef = createRef<HTMLFieldSetElement>();
const appearanceRef = createRef<HTMLSelectElement>();
export const identityControls = <>
  <Avatar name="Ada Lovelace" fallback="AL" src="/ada.png" ref={avatarRef} />
  <ToggleGroup legend="Scope" name="scope" defaultValue="all" options={[{ value: "all", label: "All" }]} ref={scopeRef} onValueChange={value => value.toUpperCase()} />
  <AppearanceSwitch label="Theme" axis="theme" options={[{ value: "application", label: "Application" }]} defaultValue="application" ref={appearanceRef} onValueChange={value => value.toUpperCase()} />
</>;
// @ts-expect-error An avatar requires a text or icon alternative.
export const unnamedAvatar = <Avatar name="Ada Lovelace" />;
// @ts-expect-error Theme is an axis value; axis only accepts the independent axis vocabulary.
export const invalidAppearanceAxis = <AppearanceSwitch label="Theme" axis="application" options={[]} />;
// @ts-expect-error Single selection is a string.
export const invalidToggleValue = <ToggleGroup legend="Scope" options={[]} value={["all"]} />;

import { parseDate } from "@internationalized/date";
import { Dialog, Drawer, AlertDialog, Popover, Tooltip, Accordion, Tabs, SearchSelect, Combobox, DateField, TimeField, DateRangeField, TimeRangeField, Calendar, RangeCalendar, DatePicker, DateRangePicker, Carousel, Navbar, NavigationMenu, NavigationScope, DropdownMenu, LinkPreview } from "../react/src";
export const requestReview = <>
  <Dialog title="Export" triggerLabel="Export" ref={createRef<HTMLDialogElement>()} actions={({ close }) => <Button onClick={close}>Cancel</Button>}><SearchSelect label="Region" options={[]} onValueChange={value => value.toUpperCase()} /></Dialog>
  <Drawer title="Trace" header={({ close }) => <Button onClick={close}>Back</Button>} />
  <AlertDialog title="Remove" confirmLabel="Remove" onConfirm={async event => { event.preventDefault(); }} />
  <Popover triggerLabel="Options" contentLabel="Options"><SearchSelect label="Regions" options={[]} multiple onValueChange={values => values.map(value => value.toUpperCase())} /></Popover>
  <Tooltip content="Explanation" child={({ props }) => <Button {...props}>Help</Button>} />
  <Accordion label="Phases" items={[{value:"query",label:"Query"}]}>{item => <p>{item.value}</p>}</Accordion>
  <Tabs label="Sections" items={[{value:"overview",label:"Overview"}]}>{item => <p>{item.value}</p>}</Tabs>
  <Combobox label="Service" options={[]} onInputValueChange={text => text.trim()} />
  <DateField label="Date" name="date" onValueChange={value => value.toUpperCase()} /><TimeField label="Time" />
  <DateRangeField label="Dates" startLabel="From" endLabel="To" onValueChange={value => value.end.trim()} /><TimeRangeField label="Times" startLabel="From" endLabel="To" />
  <Calendar label="Day" value={parseDate("2026-06-10")} ref={div} onValueChange={value => value?.add({days:1})} />
  <RangeCalendar label="Window" value={{ start: parseDate("2026-06-10"), end: undefined }} onValueChange={value => value.end?.toString()} />
  <DatePicker label="Day" calendarLabel="Choose day" /><DateRangePicker label="Window" startLabel="From" endLabel="To" calendarLabel="Choose window" />
  <Carousel label="Tips" slides={[{label:"One",content:<p>Tip</p>}]} viewport={context => <p>{context.active}</p>} />
  <Navbar brand={<strong>Monitor</strong>} links={[]} /><NavigationMenu label="Guides" items={[]} />
  <NavigationScope navigate={(href, event) => { href.trim(); event.preventDefault(); }}><LinkPreview href="#guide" label="Guide" previewLabel="Guide preview" /></NavigationScope>
  <DropdownMenu triggerLabel="Actions" contentLabel="Actions" items={[{id:"review", label:"Review", onselect:() => {}}]} />
</>;
// @ts-expect-error A multi-select callback receives an array, not a string.
<SearchSelect label="Regions" options={[]} multiple onValueChange={(value: string) => {}} />;
// @ts-expect-error Calendar values retain the portable DateValue contract.
<Calendar label="Day" value="2026-06-10" />;
// @ts-expect-error Calendar trigger and grid require distinct accessible names.
<DatePicker label="Day" />;
// @ts-expect-error Native overlay refs are HTMLDialogElement refs.
<Dialog title="Review" ref={div} />;
