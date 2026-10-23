/** Presentation of an already committed resource transfer, independent of renderer. */
export interface TransferContract {
  /** Source element ID; the source retains its own semantics. */
  fromId: string;
  /** Destination element ID; the application commits state before triggering motion. */
  toId: string;
  /** Change after a committed transfer; initial render does not animate. */
  trigger?: string | number;
  /** Presentation duration in milliseconds, bounded to 1500. */
  duration?: number;
  /** Suppress and cancel positional motion. */
  paused?: boolean;
}
