import { createRef } from "react";
import { ActionPanel, ActivityFeed, ApplicationShell, BottomNavigation, CardHeading, CategoryFilter, CategoryPreview, ChatMessage, CheckoutForm, CommandPalette, ContextMenu, Diff, Filter, Incentive, ListRow, Menubar, MultiColumnLayout, NavItem, NodeBadge, OrderHistory, OrderSummary, PageHeading, Pill, ProductFeature, ProductList, ProductOverview, ProductQuickview, Review, ShoppingCart, SidebarLayout, SidebarNavigation, SidebarPanel, SidebarRail, SpeedDial, SplitView, StoreNavigation, TaskList, Toolbar, type TaskListItem, type ShoppingCartItem, type CheckoutSection } from "../react/src";
const items = [{ id: "requests", label: "Requests", href: "/requests" }];
const tasks: readonly TaskListItem[] = [{ id: "replay", title: "Replay", state: "running", progress: 0 }];
const cart: readonly ShoppingCartItem[] = [{id:"api",name:"API",price:"$42",quantity:1,media:<span>API</span>}];
const sections: readonly CheckoutSection[] = [{legend:"Billing",fields:[{name:"company",label:"Company",inputmode:"text",autocomplete:"organization"}]}];
export const nativeApp = <ApplicationShell embedded header={<span>Workspace</span>}><SidebarLayout main={false} navigation={items} currentId="requests" sidebar={({ item, close }) => <SidebarPanel title={item?.label ?? "Requests"} toolbar={<button onClick={close}>Back</button>} />}>
 <PageHeading title="Requests" context={<span>Workspace</span>} actions={<button>New request</button>} />
 <CardHeading title="Traffic" /><ActionPanel title="Retention" actions={<button>Save</button>} />
 <ActivityFeed items={[]} /><BottomNavigation label="Workspace" items={items} currentId="requests" />
 <CategoryFilter legend="Services" options={[]} onValueChange={values=>values.map(value=>value.trim())} />
 <CategoryPreview name="Compute" href="/compute" media={0} /><ChatMessage author="Ana" avatarFallback="AN" datetime="2026-10-20" time="Now">Hello</ChatMessage>
 <CheckoutForm sections={sections} submitLabel="Continue" /><CommandPalette triggerLabel="Commands" contentLabel="Commands" label="Search" onSelect={command=>command.id.trim()} />
 <ContextMenu triggerLabel="Actions" contentLabel="Actions" /><Diff rows={[]} /><Filter legend="Environment" options={[]} onValueChange={value=>value.trim()} />
 <Incentive title="Faster" icon={<span>✓</span>} /><ListRow href="/trace" title="Trace" /><Menubar label="Editor" />
 <MultiColumnLayout start={<span>Filters</span>} end={<span>Detail</span>}>Requests</MultiColumnLayout><NavItem href="/requests" current="page" />
 <NodeBadge name="sao-01" /><OrderHistory title="Invoices" caption="History" orders={[]} /><OrderSummary title="Usage" lines={[]} totalLabel="Total" total="$0" />
 <Pill onDismiss={()=>{}}>Production</Pill><ProductFeature title="Tracing" media={<span>Trace</span>} /><ProductList label="Services" items={[]} />
 <ProductOverview title="API" /><ProductQuickview triggerLabel="Inspect" title="API" actions={({close})=><button onClick={close}>Done</button>} />
 <Review author="Ana" avatarFallback="AN" rating={4}>Useful tracing</Review><ShoppingCart title="Resources" items={cart} totalLabel="Total" total="$42" onQuantityChange={(id, quantity)=>`${id}: ${quantity.toFixed()}`} />
 <SidebarNavigation label="Workspace" items={items} currentId="requests" /><SidebarRail label="Workspace" items={items} onSelect={id=>id.trim()} />
 <SpeedDial triggerLabel="Create" contentLabel="Resources" /><SplitView fraction={0.5} onFractionChange={fraction=>fraction.toFixed(2)} />
 <StoreNavigation label="Workspace" items={items} currentId="requests" /><TaskList label="Tasks" items={tasks} actions={item=><button>{item.id}</button>} /><Toolbar label="Actions" />
</SidebarLayout></ApplicationShell>;
export const refs = <><Pill ref={createRef<HTMLSpanElement>()} /><NodeBadge name="sao" ref={createRef<HTMLSpanElement>()} /><CheckoutForm ref={createRef<HTMLFormElement>()} /><CategoryFilter legend="Services" options={[]} ref={createRef<HTMLFieldSetElement>()} /><SplitView ref={createRef<HTMLDivElement>()} /></>;
// @ts-expect-error A category image is a required content seam.
const missingMedia = <CategoryPreview name="Compute" href="/compute" />;
// @ts-expect-error Actions remain required for an action panel.
const missingAction = <ActionPanel title="Retention" />;
// @ts-expect-error Task lifecycle names follow the canonical vocabulary.
const invalidTask = <TaskList label="Tasks" items={[{id:"x",title:"Replay",state:"done"}]} />;
// @ts-expect-error Native React event attributes keep their real event payload.
const invalidEvent = <CheckoutForm onSubmit={(value: string)=>value.trim()} />;
// @ts-expect-error A span ref cannot target a form.
const invalidRef = <CheckoutForm ref={createRef<HTMLSpanElement>()} />;
void [missingMedia, missingAction, invalidTask, invalidEvent, invalidRef];

import { WorkspaceTabs, ChangeReview, ShortcutHelp, ErrorBoundary } from "../react/src";
export const recoveredWorkspace = <ErrorBoundary onfailure={error => String(error)}><WorkspaceTabs label="Documents" items={[{id:"a",label:"Draft",dirty:true}]} oncloserequest={id => id.trim()}>{item => item.id}</WorkspaceTabs><ChangeReview title="Review" items={[{id:"a",label:"Timeout",before:"1",after:"2"}]} onapplyrequest={ids => ids.map(id => id.trim())} /><ShortcutHelp title="Keys" groups={[]} /></ErrorBoundary>;
