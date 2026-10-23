export interface PaginationPage {
  /** Visible page number and stable link identity. */
  page: number;
  /** Native link destination. */
  href: string;
  /** Whether this page is currently viewed. */
  current?: boolean;
}
