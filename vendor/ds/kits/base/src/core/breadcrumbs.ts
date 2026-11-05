/** One stable item in a native breadcrumb trail. */
export interface BreadcrumbItem {
  /** Stable item identity. */
  id: string;
  /** Visible destination name. */
  label: string;
  /** Link destination; omitted, the item is plain text. */
  href?: string;
  /** Marks the current page without linking it. */
  current?: boolean;
}
