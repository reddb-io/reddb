// The application Kit's public surface.
//
// A Kit is the unit of export routing (ADR 0002): a Product Application
// receives this directory as vendorable source and imports from here. What it
// ships comes in the Taxonomy's two halves
// (.red/contexts/component-system/CONTEXT.md): a Primitive
// imports no other Kit component, and a Composite composes at least one. Both
// are mechanical properties, read off the imports by `test/taxonomy.test.ts`
// rather than decided by anyone.
//
// Each component is exported alongside its `tv()` variants object, because the
// classes are as much of the contract as the element is: a consumer that needs
// a link to look like a Button imports `button` and puts the classes on its
// own <a>, instead of forking the component to change one tag.
//
// Every export below is a named one, and nothing here has a side effect, so a
// bundler keeps only what an application actually imported — the property ADR
// 0005 asks of the Composites' behavior base, pinned by
// `test/tree-shaking.test.ts` against a real build's output.

import type { PurposeCategory } from "@reddb-io/design-system/base";

export { default as ListRow } from "./primitives/ListRow.svelte";
export { default as NavItem } from "./primitives/NavItem.svelte";
export { default as NodeBadge } from "./primitives/NodeBadge.svelte";
export { default as Pill } from "./primitives/Pill.svelte";
export { default as SplitView } from "./primitives/SplitView.svelte";

export { default as Diff } from "./composites/Diff.svelte";
export { default as CommandPalette } from "./composites/CommandPalette.svelte";
export { default as BottomNavigation } from "./composites/BottomNavigation.svelte";
export { default as ActionPanel } from "./composites/ActionPanel.svelte";
export { default as ActivityFeed } from "./composites/ActivityFeed.svelte";
export { default as ApplicationShell } from "./composites/ApplicationShell.svelte";
export { default as CardHeading } from "./composites/CardHeading.svelte";
export { default as CategoryFilter } from "./composites/CategoryFilter.svelte";
export { default as CategoryPreview } from "./composites/CategoryPreview.svelte";
export { default as ChatMessage } from "./composites/ChatMessage.svelte";
export { default as Filter } from "./composites/Filter.svelte";
export { default as Incentive } from "./composites/Incentive.svelte";
export { default as MultiColumnLayout } from "./composites/MultiColumnLayout.svelte";
export { default as ContextMenu } from "./composites/ContextMenu.svelte";
export { default as Menubar } from "./composites/Menubar.svelte";
export { default as CheckoutForm } from "./composites/CheckoutForm.svelte";
export { default as OrderHistory } from "./composites/OrderHistory.svelte";
export { default as OrderSummary } from "./composites/OrderSummary.svelte";
export { default as ShoppingCart } from "./composites/ShoppingCart.svelte";
export { default as SidebarNavigation } from "./composites/SidebarNavigation.svelte";
export { default as SidebarPanel } from "./composites/SidebarPanel.svelte";
export { sidebarPanel } from "./composites/sidebar-panel.variants";
export { default as SidebarRail } from "./composites/SidebarRail.svelte";
export { default as StoreNavigation } from "./composites/StoreNavigation.svelte";
export { default as PageHeading } from "./composites/PageHeading.svelte";
export { default as Review } from "./composites/Review.svelte";
export { default as ProductFeature } from "./composites/ProductFeature.svelte";
export { default as ProductList } from "./composites/ProductList.svelte";
export { default as ProductOverview } from "./composites/ProductOverview.svelte";
export { default as ProductQuickview } from "./composites/ProductQuickview.svelte";
export { default as SidebarLayout } from "./composites/SidebarLayout.svelte";
export { default as SpeedDial } from "./composites/SpeedDial.svelte";
export { default as TaskList } from "./composites/TaskList.svelte";
export {
  taskList,
  type TaskListVariants,
} from "./composites/task-list.variants";
export {
  TASK_STATES,
  TASK_STATE_LABELS,
  TASK_STATE_TONES,
  type TaskState,
  type TaskListItem,
} from "./composites/task-list.behavior";
export { default as Toolbar } from "./composites/Toolbar.svelte";

export {
  listRow,
  LIST_ROW_SIZES,
  type ListRowSize,
  DEPRECATED_LIST_ROW_DENSITIES,
  LIST_ROW_DENSITIES,
  type ListRowDensity,
} from "./primitives/list-row.variants";
export { navItem } from "./primitives/nav-item.variants";
export {
  nodeBadge,
  NODE_STATUSES,
  NODE_STATUS_TONES,
  type NodeStatus,
} from "./primitives/node-badge.variants";
export {
  pill,
  pillDismiss,
  PILL_SIZES,
  PILL_TONES,
  PILL_VARIANTS,
  DEPRECATED_PILL_VARIANTS,
  type DeprecatedPillVariant,
  type PillSize,
  type PillTone,
  type PillVariant,
} from "./primitives/pill.variants";
export { splitView } from "./primitives/split-view.variants";
export {
  bottomNavigation,
  type BottomNavigationVariants,
} from "./composites/bottom-navigation.variants";
export {
  actionPanel,
  type ActionPanelVariants,
} from "./composites/action-panel.variants";
export {
  activityFeed,
  type ActivityFeedVariants,
} from "./composites/activity-feed.variants";
export type { ActivityFeedItem } from "./composites/ActivityFeed.svelte";
export type { DiffRow } from "./composites/Diff.svelte";
export {
  diff,
  DIFF_MODES,
  type DiffChangeKind,
  type DiffMode,
  type DiffVariants,
} from "./composites/diff.variants";
export {
  cardHeading,
  type CardHeadingVariants,
} from "./composites/card-heading.variants";
export {
  categoryFilter,
  type CategoryFilterOption,
  type CategoryFilterVariants,
} from "./composites/category-filter.variants";
export {
  categoryPreview,
  type CategoryPreviewVariants,
} from "./composites/category-preview.variants";
export {
  chatMessage,
  type ChatMessageVariants,
} from "./composites/chat-message.variants";
export {
  checkoutForm,
  type CheckoutField,
  type CheckoutFormVariants,
  type CheckoutSection,
} from "./composites/checkout-form.variants";
export {
  DEFAULT_ORDER_HISTORY_LABELS,
  orderHistory,
  type OrderHistoryEntry,
  type OrderHistoryLabels,
  type OrderHistoryVariants,
} from "./composites/order-history.variants";
export {
  orderSummary,
  type OrderSummaryLine,
  type OrderSummaryVariants,
} from "./composites/order-summary.variants";
export {
  shoppingCart,
  type ShoppingCartItem,
  type ShoppingCartVariants,
} from "./composites/shopping-cart.variants";
export {
  filter,
  type FilterOption,
  type FilterVariants,
} from "./composites/filter.variants";
export {
  incentive,
  type IncentiveVariants,
} from "./composites/incentive.variants";
export {
  pageHeading,
  type PageHeadingVariants,
} from "./composites/page-heading.variants";
export { review, type ReviewVariants } from "./composites/review.variants";
export {
  productFeature,
  PRODUCT_FEATURE_MEDIA_SIDES,
  type ProductFeatureMediaSide,
  type ProductFeatureVariants,
} from "./composites/product-feature.variants";
export {
  productList,
  type ProductListVariants,
} from "./composites/product-list.variants";
export {
  productOverview,
  type ProductOverviewVariants,
} from "./composites/product-overview.variants";
export {
  productQuickview,
  type ProductQuickviewVariants,
} from "./composites/product-quickview.variants";
export {
  commandPalette,
  type CommandPaletteVariants,
} from "./composites/command-palette.variants";
export {
  contextMenu,
  type ContextMenuSize,
  type ContextMenuVariants,
} from "./composites/context-menu.variants";
export {
  menubar,
  type MenubarSize,
  type MenubarVariants,
} from "./composites/menubar.variants";
export {
  speedDial,
  type SpeedDialSize,
  type SpeedDialVariants,
} from "./composites/speed-dial.variants";
export {
  toolbar,
  type ToolbarSize,
  type ToolbarVariants,
} from "./composites/toolbar.variants";
export {
  sidebarNavigation,
  type SidebarNavigationVariants,
} from "./composites/sidebar-navigation.variants";
export {
  sidebarRail,
  type SidebarRailVariants,
} from "./composites/sidebar-rail.variants";
export type { SidebarRailItem } from "./composites/SidebarRail.svelte";
export {
  storeNavigation,
  type StoreNavigationVariants,
} from "./composites/store-navigation.variants";
export type {
  ApplicationNavigationItem,
  PrimaryNavigationItem,
  NavigationMarkers,
  NavigationStatus,
} from "./composites/navigation.behavior";
export {
  applicationShell,
  type ApplicationShellVariants,
} from "./composites/application-shell.variants";
export {
  shellBrandRegion,
  type ShellBrandComposition,
  type ShellBrandRegion,
} from "./composites/application-shell.behavior";
export {
  multiColumnLayout,
  type MultiColumnCount,
  type MultiColumnLayoutVariants,
} from "./composites/multi-column-layout.variants";
export {
  sidebarLayout,
  SIDEBAR_SIDES,
  type SidebarLayoutVariants,
  type SidebarSide,
} from "./composites/sidebar-layout.variants";

// SplitView's behavior, exported in its own right: an application that builds
// a splitter of its own — a three-pane layout, say — gets the same arithmetic
// rather than writing the clamping again and getting the edges wrong.
export {
  clampFraction,
  fractionAt,
  fractionForKey,
  percentOf,
  separatorOrientation,
  SPLIT_BOUNDS,
  SPLIT_ORIENTATIONS,
  SPLIT_STEP,
  type SplitBounds,
  type SplitOrientation,
} from "./primitives/split-view.behavior";

export {
  type ContextMenuEntry,
  type MenubarMenu,
  type SpeedDialAction,
  type ToolbarItem,
} from "./composites/command-surfaces.behavior";
export { type CommandPaletteCommand } from "./composites/command-palette.behavior";
export { type ProductListItem } from "./composites/product-list.behavior";

/**
 * Every Primitive this Kit ships, by component name.
 *
 * Declared rather than discovered, because a consumer reads this list out of
 * vendored source with no bundler glob to run — and pinned against the files
 * on disk by `test/taxonomy.test.ts`, so it cannot quietly fall behind. The
 * showcase renders one route per entry.
 */
export const PRIMITIVES = [
  "ListRow",
  "NavItem",
  "NodeBadge",
  "Pill",
  "SplitView",
] as const;

export type PrimitiveName = (typeof PRIMITIVES)[number];

/**
 * Every Composite this Kit ships, by component name — the same catalogue in
 * the Taxonomy's other half, kept for the same reason.
 */
export const COMPOSITES = [
  "WorkspaceTabs",
  "ChangeReview",
  "ErrorBoundary",
  "ShortcutHelp",

  "EntityInspector",
  "EntityJourney",
  "SettingsRow",
  "TaskCard",
  "TelemetryRow",
  "PropertyPanel",
  "PhaseSummary",
  "PlaybackControls",

  "ActionPanel",
  "ActivityFeed",
  "ApplicationShell",
  "BottomNavigation",
  "CardHeading",
  "CategoryFilter",
  "CategoryPreview",
  "ChatMessage",
  "CheckoutForm",
  "CommandPalette",
  "ContextMenu",
  "Diff",
  "Filter",
  "Incentive",
  "Menubar",
  "MultiColumnLayout",
  "OrderHistory",
  "OrderSummary",
  "PageHeading",
  "Review",
  "ProductFeature",
  "ProductList",
  "ProductOverview",
  "ProductQuickview",
  "SidebarLayout",
  "SidebarNavigation",
  "SidebarPanel",
  "SidebarRail",
  "ShoppingCart",
  "SpeedDial",
  "StoreNavigation",
  "TaskList",
  "Toolbar",
] as const;

export type CompositeName = (typeof COMPOSITES)[number];

/** Every component the Kit ships, of either kind. */
export const COMPONENTS = [...PRIMITIVES, ...COMPOSITES] as const;

export type ComponentName = PrimitiveName | CompositeName;

/**
 * The purpose of every application component, carried by this Kit's own
 * catalogue. Existing daisyUI sections are retained; Tailwind Plus and Bits UI
 * capabilities use the same shared purpose vocabulary.
 */
export const COMPONENT_PURPOSES = {
  WorkspaceTabs: "Navigation",
  ChangeReview: "Data display",
  ErrorBoundary: "Feedback",
  ShortcutHelp: "Navigation",
  EntityInspector: "Data display",
  EntityJourney: "Data display",
  SettingsRow: "Data input",
  TaskCard: "Data display",
  TelemetryRow: "Data display",
  PropertyPanel: "Data display",
  PhaseSummary: "Data display",
  PlaybackControls: "Actions",

  ListRow: "Data display",
  NavItem: "Navigation",
  NodeBadge: "Data display",
  Pill: "Data display",
  SplitView: "Layout",
  ActionPanel: "Actions",
  ActivityFeed: "Data display",
  ApplicationShell: "Layout",
  BottomNavigation: "Navigation",
  CardHeading: "Data display",
  CategoryFilter: "Data input",
  CategoryPreview: "Navigation",
  ChatMessage: "Data display",
  CheckoutForm: "Data input",
  CommandPalette: "Navigation",
  ContextMenu: "Actions",
  Diff: "Data display",
  Filter: "Data input",
  Incentive: "Data display",
  Menubar: "Actions",
  MultiColumnLayout: "Layout",
  OrderHistory: "Data display",
  OrderSummary: "Data display",
  PageHeading: "Data display",
  Review: "Data display",
  ProductFeature: "Data display",
  ProductList: "Data display",
  ProductOverview: "Data display",
  ProductQuickview: "Data display",
  SidebarLayout: "Layout",
  SidebarNavigation: "Navigation",
  SidebarPanel: "Navigation",
  SidebarRail: "Navigation",
  ShoppingCart: "Data input",
  SpeedDial: "Actions",
  StoreNavigation: "Navigation",
  TaskList: "Data display",
  Toolbar: "Actions",
} as const satisfies Record<ComponentName, PurposeCategory>;

export type { BottomNavigationItem } from "./composites/BottomNavigation.svelte";

export { default as TaskCard } from "./composites/TaskCard.svelte";
export { default as TelemetryRow } from "./composites/TelemetryRow.svelte";
export { default as PropertyPanel } from "./composites/PropertyPanel.svelte";
export { default as PhaseSummary } from "./composites/PhaseSummary.svelte";
export { default as PlaybackControls } from "./composites/PlaybackControls.svelte";
export type { PropertyItem, PhaseGate } from "./core/operational";

export { default as SettingsRow } from "./composites/SettingsRow.svelte";

export { default as EntityInspector } from "./composites/EntityInspector.svelte";
export type { InspectorEntity } from "./core/entity-inspector";

export { default as EntityJourney } from "./composites/EntityJourney.svelte";
export type { JourneyStage, JourneyState } from "./core/entity-journey";

export { default as WorkspaceTabs } from "./composites/WorkspaceTabs.svelte";

export { default as ChangeReview } from "./composites/ChangeReview.svelte";

export { default as ErrorBoundary } from "./composites/ErrorBoundary.svelte";

export { default as ShortcutHelp } from "./composites/ShortcutHelp.svelte";

export type { WorkspaceTab } from "./core/workspace-tabs";
export type { ReviewChange, ReviewState } from "./core/change-review";
export type { ShortcutGroup, ShortcutCommand } from "./core/shortcut-help";
