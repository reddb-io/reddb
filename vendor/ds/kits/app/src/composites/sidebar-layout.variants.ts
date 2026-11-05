import { tv, type VariantProps } from "tailwind-variants";

// Two presentations, two responsive contracts (ADR 0021).
//
//   - The panel layout (no rail) is a component placed in a page, so it is
//     container-first: the root is a named size container, and the sidebar
//     and main regions share one line in a 1 : 3 proportion from the
//     container's `3xl` step (48rem, the step the viewport `md` used to be) and
//     stack below it — beside a SplitView pane or inside a Card as on a wide
//     window. The root cannot query its own width, so the regions carry the
//     steps. The gutter is the Density layout tier (ADR 0024).
//
//   - The rail layout (a `rail` snippet) is the application's page chrome: a
//     full-height (`min-h-dvh`) frame whose compact presentation is a drawer
//     fixed to the viewport, with a viewport backdrop. ADR 0021 reserves the
//     viewport for exactly that, so its `md:`/`max-md:` steps stay. Driving the
//     drawer from container width is ADR 0021's follow-up ("Split panes"), not
//     this layout's.
const SIDE = {
  start: { sidebar: "", content: "" },
  end: { sidebar: "", content: "" },
} as const;

export const sidebarLayout = tv({
  slots: {
    root: "min-w-0 items-start",
    region: "min-w-0",
    // The panel layout's regions: a full line each below the container's
    // `3xl` step, one line in proportion from it.
    sidebar: "min-w-0 basis-full @3xl/sidebar-layout:grow-[1] @3xl/sidebar-layout:basis-0",
    content: "min-w-0 basis-full @3xl/sidebar-layout:grow-[3] @3xl/sidebar-layout:basis-0",
    railRegion: "relative z-30 min-h-dvh w-max min-w-0",
    panel: "min-w-0 border-e border-elevation-sunken-border bg-elevation-sunken-surface shadow-elevation-sunken",
    main: "min-w-0 break-words",
  },
  variants: {
    side: SIDE,
    rail: {
      false: {
        root: "@container/sidebar-layout flex w-full flex-wrap gap-[var(--reddb-spatial-layout-gap-md)]",
      },
      true: {
        root: [
          // Page chrome (see above): the viewport steps below are the frame's.
          "grid min-h-dvh items-stretch gap-0",
          "transition-[grid-template-columns] motion-reduce:transition-none",
        ].join(" "),
        panel: [
          "max-md:fixed max-md:inset-y-0 max-md:z-40 max-md:w-[min(20rem,calc(100vw_-_var(--_reddb-sidebar-rail-width,calc(var(--reddb-spatial-control-height-md)+2*var(--reddb-spatial-inset-sm)))))]",
          "max-md:overflow-y-auto max-md:p-[var(--reddb-spatial-inset-sm)]",
          "max-md:transition-transform motion-reduce:transition-none",
        ].join(" "),
      },
    },
    railOpen: {
      false: {
        railRegion: "w-0 overflow-hidden pointer-events-none md:w-0",
      },
      true: {},
    },
    panelOpen: {
      false: {
        panel: "max-md:pointer-events-none md:w-0 md:overflow-hidden md:pointer-events-none",
      },
      true: { panel: "max-md:translate-x-0" },
    },
  },
  compoundVariants: [
    {
      side: "start",
      rail: true,
      railOpen: true,
      class: {
        root: [
          "grid-cols-[auto_minmax(0,1fr)]",
        ].join(" "),
        panel: "max-md:start-[var(--_reddb-sidebar-rail-width,calc(var(--reddb-spatial-control-height-md)+2*var(--reddb-spatial-inset-sm)))]",
        main: "max-md:col-start-2",
      },
    },
    {
      side: "end",
      rail: true,
      railOpen: true,
      class: {
        root: [
          "grid-cols-[minmax(0,1fr)_auto]",
        ].join(" "),
        panel: "max-md:end-[var(--_reddb-sidebar-rail-width,calc(var(--reddb-spatial-control-height-md)+2*var(--reddb-spatial-inset-sm)))]",
        main: "max-md:col-start-1",
      },
    },
    {
      side: "start",
      rail: true,
      railOpen: false,
      class: {
        root: "grid-cols-[0_minmax(0,1fr)]",
        panel: "max-md:start-0",
        main: "max-md:col-start-2",
      },
    },
    {
      side: "end",
      rail: true,
      railOpen: false,
      class: {
        root: "grid-cols-[minmax(0,1fr)_0]",
        panel: "max-md:end-0",
        main: "max-md:col-start-1",
      },
    },
    {
      side: "start",
      rail: true,
      railOpen: true,
      panelOpen: false,
      class: {
        root: "md:grid-cols-[auto_0_minmax(0,1fr)]",
        panel: "max-md:-translate-x-full",
      },
    },
    {
      side: "start",
      rail: true,
      railOpen: true,
      panelOpen: true,
      class: {
        root: "md:grid-cols-[auto_minmax(12rem,1fr)_minmax(0,3fr)]",
      },
    },
    {
      side: "end",
      rail: true,
      railOpen: true,
      panelOpen: false,
      class: {
        root: "md:grid-cols-[minmax(0,1fr)_0_auto]",
        panel: "max-md:translate-x-full",
      },
    },
    {
      side: "end",
      rail: true,
      railOpen: true,
      panelOpen: true,
      class: {
        root: "md:grid-cols-[minmax(0,3fr)_minmax(12rem,1fr)_auto]",
      },
    },
    {
      side: "start",
      rail: true,
      railOpen: false,
      panelOpen: false,
      class: { root: "md:grid-cols-[0_0_minmax(0,1fr)]" },
    },
    {
      side: "start",
      rail: true,
      railOpen: false,
      panelOpen: true,
      class: { root: "md:grid-cols-[0_minmax(12rem,1fr)_minmax(0,3fr)]" },
    },
    {
      side: "end",
      rail: true,
      railOpen: false,
      panelOpen: false,
      class: { root: "md:grid-cols-[minmax(0,1fr)_0_0]" },
    },
    {
      side: "end",
      rail: true,
      railOpen: false,
      panelOpen: true,
      class: { root: "md:grid-cols-[minmax(0,3fr)_minmax(12rem,1fr)_0]" },
    },
  ],
  defaultVariants: { side: "start", rail: false, railOpen: true, panelOpen: false },
});

export type SidebarLayoutVariants = VariantProps<typeof sidebarLayout>;
export type SidebarSide = NonNullable<SidebarLayoutVariants["side"]>;
export const SIDEBAR_SIDES = Object.keys(SIDE) as readonly SidebarSide[];

/** Page navigation uses one item model for a desktop rail and a mobile bottom bar. */
export const responsiveNavigationLayout = tv({
  slots: {
    root: "grid min-h-dvh min-w-0 grid-cols-[minmax(0,1fr)] pb-[var(--reddb-spatial-control-height-lg)] md:grid-cols-[auto_minmax(0,1fr)] md:pb-0",
    header: "sticky top-0 z-20 col-span-full min-w-0",
    rail: "sticky top-[var(--_reddb-navigation-header-height,0px)] hidden h-dvh min-h-0 md:block",
    panel: "sticky top-[var(--_reddb-navigation-header-height,0px)] hidden h-dvh min-h-0 min-w-0 overflow-y-auto border-e border-muted bg-elevation-sunken-surface md:block",
    main: "min-w-0 p-[var(--reddb-spatial-inset-md)] [&_a]:scroll-mb-[var(--_reddb-navigation-bottom-height,0px)] [&_button]:scroll-mb-[var(--_reddb-navigation-bottom-height,0px)] [&_input]:scroll-mb-[var(--_reddb-navigation-bottom-height,0px)]",
    bottom: "fixed inset-x-0 bottom-0 z-30 md:hidden",
    more: "grid gap-[var(--reddb-spatial-gap-sm)]",
    drawer: "max-w-[min(42ch,calc(100vw_-_var(--reddb-spatial-inset-lg)))] pb-[max(var(--reddb-spatial-inset-md),env(safe-area-inset-bottom))]",
    menu: "min-h-0 min-w-0",
  },
  variants: {
    header: {true:{root:"grid-rows-[auto_minmax(0,1fr)]"},false:{root:"grid-rows-[minmax(0,1fr)]"}},
    dockable: {true:{},false:{root:"grid-cols-[auto_minmax(0,1fr)] pb-0",rail:"block"}},
    panel: { true: {root: "md:grid-cols-[auto_minmax(14rem,20rem)_minmax(0,1fr)]"}, false: {} },
    side: {start: {}, end: {}},
  },
  compoundVariants: [
    {side:"end",panel:false,class:{root:"md:grid-cols-[minmax(0,1fr)_auto]",rail:"md:order-3",main:"md:order-1"}},
    {side:"end",panel:true,class:{root:"md:grid-cols-[minmax(0,1fr)_minmax(14rem,20rem)_auto]",rail:"md:order-3",panel:"md:order-2",main:"md:order-1"}},
  ],
  defaultVariants: {panel:false,side:"start",dockable:true,header:false},
});
