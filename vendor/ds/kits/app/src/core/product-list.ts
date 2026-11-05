
/** Caller-owned content for one entry in the canonical product collection. */
export interface ProductListItem<Content = unknown> {
  /** Stable identity used by the keyed collection. */
  id: string;
  /** Visible product name and the accessible name of its optional link. */
  name: string;
  /** Optional canonical product destination. */
  href?: string;
  description?: string;
  price?: string;
  /** Caller-owned product imagery, including its own alternative text. */
  media?: Content;
  /** Caller-owned canonical controls, kept after the product identity. */
  actions?: Content;
}
