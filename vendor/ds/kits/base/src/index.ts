// The Base Kit's public surface.
//
// The Base Kit is the parent every other Kit declares (ADR 0004): a consumer
// that declares any Kit receives this one too, so a component here reaches
// every application without it pulling a sibling Kit. That makes the bar for
// entry higher than the application Kit's, not lower — what belongs here is
// what is cross-audience by nature, the Logo first.
//
// The list below is the seam a component arrives through, pinned against the
// files on disk by `test/components.test.ts` so a component added here cannot
// go unexported.

export { default as Accordion } from "./Accordion.svelte";
export { default as Alert } from "./Alert.svelte";
export { default as AlertDialog } from "./AlertDialog.svelte";
export { default as AppearanceSwitch } from "./AppearanceSwitch.svelte";
export { default as AspectRatio } from "./AspectRatio.svelte";
export { default as Avatar } from "./Avatar.svelte";
export { default as Badge } from "./Badge.svelte";
export { default as BrowserMockup } from "./BrowserMockup.svelte";
export { default as Button } from "./Button.svelte";
export { default as Breadcrumbs } from "./Breadcrumbs.svelte";
export { default as BulletsSlide } from "./BulletsSlide.svelte";
export { default as Calendar } from "./Calendar.svelte";
export { default as Card } from "./Card.svelte";
export { default as Carousel } from "./Carousel.svelte";
export { default as Checkbox } from "./Checkbox.svelte";
export { default as CodeBlock } from "./CodeBlock.svelte";
export { default as Combobox } from "./Combobox.svelte";
export { default as Container } from "./Container.svelte";
export { default as ContentMask } from "./ContentMask.svelte";
export { default as ControlGroup } from "./ControlGroup.svelte";
export { default as Countdown } from "./Countdown.svelte";
export { default as DateField } from "./DateField.svelte";
export { default as DatePicker } from "./DatePicker.svelte";
export { default as DateRangeField } from "./DateRangeField.svelte";
export { default as DateRangePicker } from "./DateRangePicker.svelte";
export { default as Deck } from "./Deck.svelte";
export { default as DescriptionList } from "./DescriptionList.svelte";
export { default as Dialog } from "./Dialog.svelte";
export { default as Disclosure } from "./Disclosure.svelte";
export { default as Divider } from "./Divider.svelte";
export { default as Drawer } from "./Drawer.svelte";
export { default as DropdownMenu } from "./DropdownMenu.svelte";
export { default as EmptyState } from "./EmptyState.svelte";
export { default as Eyebrow } from "./Eyebrow.svelte";
export { default as Field } from "./Field.svelte";
export { default as Fieldset } from "./Fieldset.svelte";
export { default as FigureSlide } from "./FigureSlide.svelte";
export { default as FileInput } from "./FileInput.svelte";
export { default as Form } from "./Form.svelte";
export { default as GridList } from "./GridList.svelte";
export { default as Heading } from "./Heading.svelte";
export { default as Icon } from "./Icon.svelte";
export { default as Image } from "./Image.svelte";
export { default as Indicator } from "./Indicator.svelte";
export { default as InlineEdit } from "./InlineEdit.svelte";
export { inlineEdit } from "./inline-edit.variants";
export { default as Input } from "./Input.svelte";
export { default as Kbd } from "./Kbd.svelte";
export { default as Label } from "./Label.svelte";
export { default as Link } from "./Link.svelte";
export { default as LinkPreview } from "./LinkPreview.svelte";
export { default as List } from "./List.svelte";
export { default as ListContainer } from "./ListContainer.svelte";
export { default as LoadingIndicator } from "./LoadingIndicator.svelte";
export { default as Logo } from "./Logo.svelte";
export { default as MediaObject } from "./MediaObject.svelte";
export { default as Meter } from "./Meter.svelte";
export { default as Navbar } from "./Navbar.svelte";
export { default as NavigationMenu } from "./NavigationMenu.svelte";
export { default as NavigationScope } from "./NavigationScope.svelte";
export { default as PhoneInput } from "./PhoneInput.svelte";
export type { PhoneCountry } from "./core/contracts/phoneinput";
export { phoneInput } from "./phone-input.variants";
export { default as NumberField } from "./NumberField.svelte";
export { numberField, type NumberFieldVariants } from "./number-field.variants";
export { default as Notification } from "./Notification.svelte";
export { default as OneTimeCodeInput } from "./OneTimeCodeInput.svelte";
export { default as Pagination } from "./Pagination.svelte";
export { default as PhoneMockup } from "./PhoneMockup.svelte";
export { default as Popover } from "./Popover.svelte";
export { default as Progress } from "./Progress.svelte";
export { default as Prose } from "./Prose.svelte";
export { default as QuoteSlide } from "./QuoteSlide.svelte";
export { default as RadialProgress } from "./RadialProgress.svelte";
export { default as RadioGroup } from "./RadioGroup.svelte";
export { default as RangeCalendar } from "./RangeCalendar.svelte";
export { default as Rating } from "./Rating.svelte";
export { default as Reveal } from "./Reveal.svelte";
export { default as ScrollArea } from "./ScrollArea.svelte";
export { default as SectionHeading } from "./SectionHeading.svelte";
export { default as SectionSlide } from "./SectionSlide.svelte";
export { default as SearchSelect } from "./SearchSelect.svelte";
export {
  searchSelect,
  searchSelectOptions,
  type SearchSelectOption,
} from "./search-select.variants";
export { default as Select } from "./Select.svelte";
export type { SelectOption } from "./Select.svelte";
export { default as Skeleton } from "./Skeleton.svelte";
export { default as SkipLink } from "./SkipLink.svelte";
export { default as SlideFrame } from "./SlideFrame.svelte";
export { default as Slider } from "./Slider.svelte";
export { default as Stack } from "./Stack.svelte";
export { default as Statistic } from "./Statistic.svelte";
export { default as Steps } from "./Steps.svelte";
export { default as StatusIndicator } from "./StatusIndicator.svelte";
export { default as Swap } from "./Swap.svelte";
export { default as Switch } from "./Switch.svelte";
export { default as Table } from "./Table.svelte";
export { default as Tabs } from "./Tabs.svelte";
export { default as Text } from "./Text.svelte";
export { default as Textarea } from "./Textarea.svelte";
export { default as TimeField } from "./TimeField.svelte";
export { default as TimeRangeField } from "./TimeRangeField.svelte";
export { default as Timeline } from "./Timeline.svelte";
export { default as TitleSlide } from "./TitleSlide.svelte";
export { default as ToggleButton } from "./ToggleButton.svelte";
export { default as ToggleGroup } from "./ToggleGroup.svelte";
export { default as Tooltip } from "./Tooltip.svelte";
export { default as TwoColumnSlide } from "./TwoColumnSlide.svelte";
export { default as WindowMockup } from "./WindowMockup.svelte";

// The one semantic vocabulary (ADR 0026): every Kit imports `tone` from here.
export {
  FEEDBACK_TONES,
  TONES,
  isTone,
  type FeedbackTone,
  type Tone,
} from "./tone";
// The deprecation window and the type-role merger, for child Kits that wear
// them too (ADR 0025, 0026): one copy, reached through the public barrel.
export { warnDeprecated } from "./deprecation";
// Its sibling for the accessible-name contract (wave 5A): one dev-only warning
// per broken contract, the same voice, reachable by every child Kit.
export { warnA11y } from "./a11y";
export { TYPE_ROLE_UTILITIES, typeRoleMerge } from "./type-roles";
// Structured Data (ADR 0028): the safe JSON-LD serializer and the
// BreadcrumbList builder Breadcrumbs emits through. The Marketing Kit builds
// every other schema.org entity on the same serializer.
export {
  SCHEMA_ORG,
  absoluteUrl,
  breadcrumbList,
  isAbsoluteUrl,
  jsonLdScript,
  serializeJsonLd,
  type BreadcrumbListInput,
  type BreadcrumbListOptions,
  type JsonLdDocument,
  type JsonLdNode,
} from "./structured-data";

export {
  REDUCED_MOTION_QUERY,
  useReducedMotion,
  type ReducedMotionPreference,
} from "./reduced-motion.svelte";
export { reveal, type RevealVariants } from "./reveal.variants";
export {
  getNavigation,
  ownsSameOrigin,
  routableAnchor,
  setNavigation,
  type NavigationSeam,
} from "./navigation";

/**
 * Every component this Kit ships, by component name.
 *
 * Declared rather than discovered, because a consumer reads this list out of
 * vendored source with no bundler glob to run — the same contract the
 * application Kit's `PRIMITIVES` carries.
 */
export const BASE_COMPONENTS = [
  "Accordion",
  "Alert",
  "AlertDialog",
  "AppearanceSwitch",
  "AspectRatio",
  "Avatar",
  "Badge",
  "Breadcrumbs",
  "BrowserMockup",
  "BulletsSlide",
  "Button",
  "Calendar",
  "Card",
  "Carousel",
  "Checkbox",
  "CodeBlock",
  "Combobox",
  "Container",
  "ContentMask",
  "ControlGroup",
  "Countdown",
  "DateField",
  "DatePicker",
  "DateRangeField",
  "DateRangePicker",
  "Deck",
  "DescriptionList",
  "Dialog",
  "Disclosure",
  "Divider",
  "Drawer",
  "DropdownMenu",
  "EmptyState",
  "Eyebrow",
  "Field",
  "Fieldset",
  "FigureSlide",
  "FileInput",
  "Form",
  "GridList",
  "Heading",
  "Icon",
  "Image",
  "Indicator",
  "InlineEdit",
  "Input",
  "Kbd",
  "Label",
  "Link",
  "LinkPreview",
  "List",
  "ListContainer",
  "LoadingIndicator",
  "Logo",
  "MediaObject",
  "Meter",
  "Navbar",
  "NavigationMenu",
  "NavigationScope",
  "Notification",
  "NumberField",
  "OneTimeCodeInput",
  "Pagination",
  "PhoneInput",
  "PhoneMockup",
  "Popover",
  "Progress",
  "Prose",
  "QuoteSlide",
  "RadialProgress",
  "RadioGroup",
  "RangeCalendar",
  "Rating",
  "Reveal",
  "ScrollArea",
  "SearchSelect",
  "SecretValue",
  "SectionHeading",
  "SectionSlide",
  "Select",
  "Skeleton",
  "SkipLink",
  "SlideFrame",
  "Slider",
  "Stack",
  "Statistic",
  "StatusIndicator",
  "Steps",
  "Swap",
  "Switch",
  "Table",
  "Tabs",
  "Text",
  "Textarea",
  "TimeField",
  "TimeRangeField",
  "Timeline",
  "TitleSlide",
  "ToggleButton",
  "ToggleGroup",
  "Tooltip",
  "Transfer",
  "TwoColumnSlide",
  "WindowMockup",
] as const;

export type BaseComponentName = (typeof BASE_COMPONENTS)[number];

/** The purpose vocabulary shared by every Kit catalogue. */
export const PURPOSE_CATEGORIES = [
  "Actions",
  "Data input",
  "Data display",
  "Navigation",
  "Feedback",
  "Layout",
  "Mockup",
] as const;

export type PurposeCategory = (typeof PURPOSE_CATEGORIES)[number];

/** Reader intent for each purpose group, used when placing new components. */
export const PURPOSE_GROUP_MEANINGS = {
  Actions: "Controls that initiate or confirm operations and decisions.",
  "Data input":
    "Components that collect, edit, select, or validate user-provided values.",
  "Data display":
    "Components that present, summarize, compare, or organize information.",
  Navigation:
    "Components that move readers among destinations or orient them within a structure.",
  Feedback:
    "Components that communicate status, progress, outcomes, or required attention.",
  Layout:
    "Components that arrange page regions, content, and related elements.",
  Mockup:
    "Components that frame illustrative content inside recognizable device or code contexts.",
} as const satisfies Record<PurposeCategory, string>;

/**
 * The purpose of every Base component.
 *
 * daisyUI's catalogue supplies its section names directly. Components that
 * only occur in Bits UI or Tailwind Plus are assigned to the same vocabulary
 * here, at the Kit boundary, so consumers never need a parallel category map.
 */
export const BASE_COMPONENT_PURPOSES = {
  Transfer: "Layout",
  Accordion: "Data display",
  Alert: "Feedback",
  AlertDialog: "Actions",
  AppearanceSwitch: "Actions",
  AspectRatio: "Layout",
  Avatar: "Data display",
  Badge: "Data display",
  Breadcrumbs: "Navigation",
  BrowserMockup: "Mockup",
  BulletsSlide: "Layout",
  Button: "Actions",
  Calendar: "Data input",
  Card: "Data display",
  Carousel: "Data display",
  Checkbox: "Data input",
  CodeBlock: "Mockup",
  Combobox: "Data input",
  Container: "Layout",
  ContentMask: "Layout",
  ControlGroup: "Layout",
  Countdown: "Data display",
  DateField: "Data input",
  DatePicker: "Data input",
  DateRangeField: "Data input",
  DateRangePicker: "Data input",
  Deck: "Layout",
  DescriptionList: "Data display",
  Dialog: "Actions",
  Disclosure: "Data display",
  Divider: "Layout",
  Drawer: "Layout",
  DropdownMenu: "Actions",
  EmptyState: "Feedback",
  Eyebrow: "Data display",
  Field: "Data input",
  Fieldset: "Data input",
  FigureSlide: "Layout",
  FileInput: "Data input",
  Form: "Data input",
  GridList: "Data display",
  Heading: "Data display",
  Icon: "Data display",
  Image: "Data display",
  Indicator: "Layout",
  InlineEdit: "Data input",
  Input: "Data input",
  Kbd: "Data display",
  Label: "Data input",
  Link: "Navigation",
  LinkPreview: "Data display",
  List: "Data display",
  ListContainer: "Layout",
  LoadingIndicator: "Feedback",
  Logo: "Data display",
  MediaObject: "Layout",
  Meter: "Data display",
  Navbar: "Navigation",
  NavigationMenu: "Navigation",
  NavigationScope: "Navigation",
  Notification: "Feedback",
  NumberField: "Data input",
  PhoneInput: "Data input",
  OneTimeCodeInput: "Data input",
  Pagination: "Navigation",
  PhoneMockup: "Mockup",
  Popover: "Actions",
  Progress: "Feedback",
  Prose: "Data display",
  QuoteSlide: "Layout",
  RadialProgress: "Feedback",
  RadioGroup: "Data input",
  RangeCalendar: "Data input",
  Rating: "Data input",
  Reveal: "Layout",
  ScrollArea: "Layout",
  SectionHeading: "Data display",
  SectionSlide: "Layout",
  SecretValue: "Data display",
  SearchSelect: "Data input",
  Select: "Data input",
  Skeleton: "Feedback",
  SkipLink: "Navigation",
  SlideFrame: "Layout",
  Slider: "Data input",
  Stack: "Layout",
  Statistic: "Data display",
  StatusIndicator: "Data display",
  Steps: "Navigation",
  Swap: "Actions",
  Switch: "Data input",
  Table: "Data display",
  Tabs: "Navigation",
  Text: "Data display",
  Textarea: "Data input",
  TimeField: "Data input",
  TimeRangeField: "Data input",
  Timeline: "Data display",
  TitleSlide: "Layout",
  ToggleButton: "Actions",
  ToggleGroup: "Actions",
  Tooltip: "Feedback",
  TwoColumnSlide: "Layout",
  WindowMockup: "Mockup",
} as const satisfies Record<BaseComponentName, PurposeCategory>;

export type { BreadcrumbItem } from "./Breadcrumbs.svelte";
export { breadcrumbs, type BreadcrumbsVariants } from "./breadcrumbs.variants";
export {
  arrangements,
  NAVBAR_ALIGNMENTS,
  NAVBAR_COLLAPSE,
  type NavbarArrangements,
  type NavbarAlign,
  type NavbarCollapse,
  type NavbarLink,
} from "./Navbar.svelte";
export { navbar, type NavbarVariants } from "./navbar.variants";
export type { PaginationPage } from "./Pagination.svelte";
export { pagination, type PaginationVariants } from "./pagination.variants";
export {
  DEFAULT_PHONE_MOCKUP_RATIO,
  phoneMockup,
  type PhoneMockupVariants,
} from "./phone-mockup.variants";
export {
  DEFAULT_WINDOW_MOCKUP_RATIO,
  windowMockup,
  type WindowMockupVariants,
} from "./window-mockup.variants";
export type { StepItem, StepState } from "./Steps.svelte";
export { steps, type StepsVariants } from "./steps.variants";
export type { CarouselSlide, CarouselViewportContext } from "./Carousel.svelte";
export type {
  TableCellContext,
  TableCellValue,
  TableColumn,
  TableRow,
} from "./Table.svelte";

/** Components that compose another canonical Base component. */
export const BASE_COMPOSITES = [
  "Accordion",
  "AlertDialog",
  "AppearanceSwitch",
  "Avatar",
  "Breadcrumbs",
  "BrowserMockup",
  "BulletsSlide",
  "Calendar",
  "Card",
  "Carousel",
  "Checkbox",
  "CodeBlock",
  "Combobox",
  "DateField",
  "DatePicker",
  "DateRangeField",
  "DateRangePicker",
  "Drawer",
  "DropdownMenu",
  "Disclosure",
  "Field",
  "FigureSlide",
  "FileInput",
  "GridList",
  "InlineEdit",
  "ListContainer",
  "Navbar",
  "NavigationMenu",
  "Notification",
  "NumberField",
  "OneTimeCodeInput",
  "Pagination",
  "PhoneInput",
  "PhoneMockup",
  "Popover",
  "QuoteSlide",
  "RadioGroup",
  "RangeCalendar",
  "Rating",
  "SearchSelect",
  "SecretValue",
  "SectionSlide",
  "Slider",
  "Statistic",
  "Steps",
  "Swap",
  "Switch",
  "Tabs",
  "TimeField",
  "TimeRangeField",
  "Timeline",
  "TitleSlide",
  "ToggleButton",
  "ToggleGroup",
  "TwoColumnSlide",
  "WindowMockup",
] as const satisfies readonly BaseComponentName[];
export type BaseCompositeName = (typeof BASE_COMPOSITES)[number];

/** Components with no imports from another Kit component. */
export type BasePrimitiveName = Exclude<BaseComponentName, BaseCompositeName>;
const baseCompositeNames = new Set<BaseComponentName>(BASE_COMPOSITES);
export const BASE_PRIMITIVES = BASE_COMPONENTS.filter(
  (name): name is BasePrimitiveName => !baseCompositeNames.has(name),
);

export {
  ICON_COLORS,
  ICON_SIZES,
  type IconColor,
  type IconGlyph,
  type IconGlyphProps,
  type IconSize,
} from "./Icon.svelte";

export { calendar, type CalendarVariants } from "./calendar.variants";
export { dateField, type DateFieldVariants } from "./date-field.variants";
export { datePicker, type DatePickerVariants } from "./date-picker.variants";
export {
  dateRangeField,
  type DateRangeFieldVariants,
} from "./date-range-field.variants";
export {
  dateRangePicker,
  type DateRangePickerVariants,
} from "./date-range-picker.variants";

export {
  accordion,
  type AccordionItem,
  type AccordionVariants,
} from "./accordion.variants";

export {
  DEFAULT_ASPECT_RATIO,
  aspectRatio,
  type AspectRatioVariants,
} from "./aspect-ratio.variants";

export {
  browserMockup,
  DEFAULT_BROWSER_MOCKUP_RATIO,
  type BrowserMockupVariants,
} from "./browser-mockup.variants";

export {
  AVATAR_SIZES,
  avatar,
  type AvatarSize,
  type AvatarVariants,
} from "./avatar.variants";

export {
  CARD_ACTIONS_ALIGNMENTS,
  CARD_MEDIA_FITS,
  CARD_MEDIA_POSITIONS,
  CARD_MEDIA_RATIOS,
  CARD_MEDIA_SPANS,
  CARD_ORIENTATIONS,
  CARD_PADDINGS,
  CARD_TONES,
  CARD_VARIANTS,
  DEPRECATED_CARD_TONES,
  card,
  type CardActionsAlign,
  type CardMediaFit,
  type CardMediaPosition,
  type CardMediaRatio,
  type CardMediaSpan,
  type CardOrientation,
  type CardPadding,
  type CardTone,
  type CardVariant,
  type CardVariants,
  type DeprecatedCardTone,
} from "./card.variants";

export { carousel, type CarouselVariants } from "./carousel.variants";

export {
  CONTAINER_SIZES,
  container,
  type ContainerSize,
  type ContainerVariants,
} from "./container.variants";

export {
  CONTENT_MASK_CLIP_PATHS,
  CONTENT_MASK_SHAPES,
  contentMask,
  type ContentMaskShape,
  type ContentMaskVariants,
} from "./content-mask.variants";

export {
  DIVIDER_ORIENTATIONS,
  divider,
  type DividerOrientation,
  type DividerVariants,
} from "./divider.variants";

export {
  CONTROL_GROUP_ORIENTATIONS,
  controlGroup,
  type ControlGroupOrientation,
  type ControlGroupVariants,
} from "./control-group.variants";

export {
  STACK_ALIGNMENTS,
  STACK_DIRECTIONS,
  STACK_GAPS,
  STACK_JUSTIFICATIONS,
  stack,
  type StackAlign,
  type StackDirection,
  type StackGap,
  type StackJustify,
  type StackVariants,
} from "./stack.variants";

export {
  LIST_GAPS,
  list,
  type ListGap,
  type ListVariants,
} from "./list.variants";

export {
  GRID_LIST_COLUMNS,
  gridList,
  type GridListColumn,
  type GridListVariants,
} from "./grid-list.variants";

export { skipLink, type SkipLinkVariants } from "./skip-link.variants";
export { statistic, type StatisticVariants } from "./statistic.variants";
export { switchControl, type SwitchVariants } from "./switch.variants";
export { table, type TableVariants } from "./table.variants";
export type { TimelineItem } from "./Timeline.svelte";
export { timeline, type TimelineVariants } from "./timeline.variants";
export {
  mediaObject,
  type MediaObjectAlign,
  type MediaObjectGap,
  type MediaObjectVariants,
} from "./media-object.variants";
export { tabs, type TabItem, type TabsVariants } from "./tabs.variants";
export { disclosure, type DisclosureVariants } from "./disclosure.variants";

export {
  formatPercent,
  isDeterminate,
  normalizeRange,
  type NormalizedRange,
} from "./progress.behavior";
export { progress, type ProgressVariants } from "./progress.variants";
export {
  IMAGE_FITS,
  IMAGE_POSITIONS,
  assertImageContract,
  image,
  imageRatio,
  type ImageAsset,
  type ImageFit,
  type ImagePosition,
  type ImageSource,
  type ImageVariants,
} from "./image.variants";
export {
  PROSE_ELEMENTS,
  PROSE_MEASURES,
  prose,
  type ProseElement,
  type ProseMeasure,
  type ProseVariants,
} from "./prose.variants";
export { formatDuration, normalizeRemaining } from "./countdown.behavior";
export { countdown, type CountdownVariants } from "./countdown.variants";
export {
  RADIAL_PROGRESS_SIZES,
  radialProgress,
  type RadialProgressSize,
  type RadialProgressVariants,
} from "./radial-progress.variants";
export {
  meter,
  METER_TONES,
  type DeprecatedMeterTone,
  type MeterTone,
  type MeterVariants,
} from "./meter.variants";
export {
  LOADING_INDICATOR_SIZES,
  loadingIndicator,
  type LoadingIndicatorSize,
  type LoadingIndicatorVariants,
} from "./loading-indicator.variants";
export {
  SKELETON_SHAPES,
  skeleton,
  type SkeletonShape,
  type SkeletonVariants,
} from "./skeleton.variants";

export {
  ALERT_FEEDBACK_ROLES,
  ALERT_TONES,
  alert,
  type AlertFeedbackRole,
  type AlertTone,
  type AlertVariants,
} from "./alert.variants";

export {
  BADGE_VARIANTS,
  DEPRECATED_BADGE_VARIANTS,
  badge,
  type BadgeTone,
  type BadgeVariant,
  type BadgeVariants,
  type DeprecatedBadgeVariant,
} from "./badge.variants";

// Button's variants and enumerated axes are Extension Seams: a consumer can
// compose the canonical appearance onto its own element or build a local
// specialization without copying the component's private implementation.
export {
  BUTTON_INTENTS,
  BUTTON_SIZES,
  BUTTON_TONES,
  BUTTON_VARIANTS,
  button,
  buttonSpinner,
  type ButtonIntent,
  type ButtonSize,
  type ButtonTone,
  type ButtonVariant,
  type ButtonVariants,
} from "./button.variants";

// The interaction-state contract every quiet control draws (wave 6A): rest,
// hover, pressed, open, focus, disabled and selected in one recipe. Button's
// ghost variant is built on it; a child Kit wraps a raw control's classes in
// it so its states never drift from Button's.
export {
  QUIET_CONTROL_FOCUS,
  QUIET_CONTROL_INKS,
  QUIET_DISABLED,
  QUIET_FOCUS,
  QUIET_HOVER,
  QUIET_OPEN,
  QUIET_PRESSED,
  quietControl,
  type QuietControlFocus,
  type QuietControlInk,
  type QuietControlVariants,
} from "./quiet-control.variants";

export { checkbox, type CheckboxVariants } from "./checkbox.variants";
export {
  CODE_TOKEN_CLASSES,
  codeBlock,
  codeTokens,
  type CodeBlockVariants,
  type CodeToken,
  type CodeTokenizer,
} from "./code-block.variants";
export {
  combobox,
  type ComboboxOption,
  type ComboboxVariants,
} from "./combobox.variants";

export {
  DIALOG_SIZES,
  dialog,
  dialogClose,
  type DialogSize,
  type DialogVariants,
} from "./dialog.variants";
export {
  DESCRIPTION_LIST_GAPS,
  descriptionList,
  type DescriptionListContent,
  type DescriptionListGap,
  type DescriptionListItem,
  type DescriptionListVariants,
} from "./description-list.variants";
export {
  dropdownMenu,
  DROPDOWN_MENU_SIZES,
  type DropdownMenuSize,
  type DropdownMenuVariants,
} from "./dropdown-menu.variants";
export {
  dropdownMenuGroups,
  dropdownMenuItems,
  isDropdownMenuGroup,
  type DropdownMenuEntry,
  type DropdownMenuGroup,
  type DropdownMenuItem,
} from "./dropdown-menu.behavior";
export { linkPreview, type LinkPreviewVariants } from "./link-preview.variants";
export {
  navigationMenu,
  type NavigationMenuOrientation,
  type NavigationMenuSize,
  type NavigationMenuVariants,
} from "./navigation-menu.variants";
export {
  isNavigationMenuSection,
  type NavigationMenuEntry,
  type NavigationMenuLink,
  type NavigationMenuSection,
} from "./navigation-menu.behavior";
export {
  notification,
  type NotificationVariants,
} from "./notification.variants";
export { popover, type PopoverVariants } from "./popover.variants";
export {
  APPEARANCE_AXES,
  applyAppearance,
  appearanceAttribute,
  appearanceStorageKey,
  type AppearanceAxis,
  type AppearanceOption,
  type AppearanceStorage,
} from "./appearance-switch.behavior";

export {
  alertDialog,
  alertDialogActions,
  type AlertDialogVariants,
} from "./alert-dialog.variants";

export {
  DRAWER_SIDES,
  drawer,
  type DrawerSide,
  type DrawerVariants,
} from "./drawer.variants";
export {
  EMPTY_STATE_SIZES,
  emptyState,
  type EmptyStateSize,
  type EmptyStateVariants,
} from "./empty-state.variants";

// Field passes this complete typed association to its control snippet. The
// appearance functions are the corresponding optional Extension Seams.
export {
  field,
  FIELD_LAYOUTS,
  type FieldControlProps,
  type FieldLayout,
  type FieldVariants,
} from "./field.variants";
export {
  fieldset,
  type FieldsetControlProps,
  type FieldsetVariants,
} from "./fieldset.variants";
export { fileInput, type FileInputVariants } from "./file-input.variants";
export { form, type FormVariants } from "./form.variants";
export { input, type InputVariants } from "./input.variants";
export {
  INDICATOR_POSITIONS,
  indicator,
  type IndicatorPosition,
  type IndicatorVariants,
} from "./indicator.variants";
export { kbd, kbdChord, KBD_SIZES, type KbdSize } from "./kbd.variants";
export { label, type LabelVariants } from "./label.variants";
export { link, type LinkVariants } from "./link.variants";
export {
  oneTimeCodeInput,
  type OneTimeCodeInputVariants,
} from "./one-time-code-input.variants";
export {
  radioGroup,
  type RadioGroupOption,
  type RadioGroupVariants,
} from "./radio-group.variants";
export { rating, type RatingVariants } from "./rating.variants";
export {
  SCROLL_AREA_ORIENTATIONS,
  scrollArea,
  type ScrollAreaOrientation,
  type ScrollAreaVariants,
} from "./scroll-area.variants";
export {
  SECTION_HEADING_LEVELS,
  SECTION_HEADING_SIZES,
  sectionHeading,
  type SectionHeadingLevel,
  type SectionHeadingSize,
  type SectionHeadingVariants,
} from "./section-heading.variants";
export { select, type SelectVariants } from "./select.variants";
export { slider, type SliderVariants } from "./slider.variants";
export {
  STATUS_INDICATOR_STATUSES,
  STATUS_INDICATOR_TONES,
  statusIndicator,
  type StatusIndicatorStatus,
  type StatusIndicatorTone,
  type StatusIndicatorVariants,
} from "./status-indicator.variants";
export { textarea, type TextareaVariants } from "./textarea.variants";
export { timeField, type TimeFieldVariants } from "./time-field.variants";
export {
  timeRangeField,
  type TimeRangeFieldVariants,
} from "./time-range-field.variants";
export {
  toggleButton,
  type ToggleButtonVariants,
} from "./toggle-button.variants";
export { swap, type SwapVariants } from "./swap.variants";
export {
  toggleGroup,
  type ToggleGroupOption,
  type ToggleGroupVariants,
} from "./toggle-group.variants";
export { tooltip, type TooltipVariants } from "./tooltip.variants";

// The Logo's own vocabulary, exported because a caller has to be able to
// enumerate what it may ask for — a showcase rendering the whole matrix, an
// application offering a layout picker — without restating a list that would
// then drift from the Marks the pinned release actually ships.
export {
  BRAND_RELEASE,
  LOGO_LAYOUTS,
  LOGO_SURFACES,
  MARKS,
  MARK_CATALOGUE,
  MarkNotShippedError,
  selectMark,
  shipsMark,
} from "./logo.marks";
export type { LogoLayout, LogoSurface, Mark } from "./logo.marks";

// …and its physics, for the same reason: clearspace and the minimum are the
// component's to enforce, but a caller laying out around it may need the box.
export {
  CLEARSPACE_RATIO,
  DEFAULT_SYMBOL_PX,
  LogoTooSmallError,
  MINIMUM_SYMBOL_PX,
  logoBox,
  minimumSize,
} from "./logo.box";
export type { LogoBox } from "./logo.box";

export {
  COLOR_SCHEME_ATTRIBUTE,
  activeSurface,
  isLogoSurface,
  observeActiveSurface,
  surfaceOfColorScheme,
} from "./logo.surface";
export { logo, logoMark } from "./logo.variants";

// The type Primitives (ADR 0025): they consume only the Theme's type roles.
export {
  HEADING_LEVELS,
  HEADING_ROLES,
  defaultHeadingRole,
  heading,
  type HeadingLevel,
  type HeadingRole,
  type HeadingVariants,
} from "./heading.variants";
export {
  TEXT_ELEMENTS,
  TEXT_INKS,
  TEXT_ROLES,
  TEXT_TONES,
  defaultTextInk,
  defaultTextTone,
  text,
  type TextElement,
  type TextInk,
  type TextRole,
  type TextTone,
  type TextVariants,
} from "./text.variants";
export {
  EYEBROW_ELEMENTS,
  eyebrow,
  type EyebrowElement,
  type EyebrowVariants,
} from "./eyebrow.variants";

export {
  SLIDE_CANVAS_WIDTH_REM,
  SLIDE_SURFACES,
  slideFrame,
  type SlideFrameVariants,
  type SlideSurface,
} from "./slide-frame.variants";
export { slideLayout, type SlideLayoutVariants } from "./slide-layout.variants";
export { deck, type DeckVariants } from "./deck.variants";

export {
  provideNavigationDisclosures,
  navigationDisclosure,
} from "./navigation";
export { portalAppearance } from "./portal-appearance.svelte";

export { afterFormReset, associatedForm } from "./core/native-controls";

export { default as Transfer } from "./Transfer.svelte";
export { observeActivity } from "./core/operational-motion";

export { default as SecretValue } from "./SecretValue.svelte";
export type { SecretValueContract, SecretHideReason } from "./core/contracts/secretvalue";
