/** Caller-owned product data used by one editable cart row. */
export interface ShoppingCartItem<Content = unknown> {
  id: string;
  name: string;
  description?: string;
  price: string;
  quantity: number;
  quantityName?: string;
  disabled?: boolean;
  /** Caller-owned product media with its own accessible alternative. */
  media?: Content;
  /** Caller-owned detail rendering for configurations or other product metadata. */
  details?: Content;
  /** Caller-owned per-item actions rendered after the canonical quantity control. */
  actions?: Content;
}

