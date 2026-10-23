export type NavbarCollapse = "responsive" | "expanded" | "collapsed";
export type NavbarAlign = "start" | "center";
export const NAVBAR_ALIGNMENTS = ["start", "center"] as const satisfies readonly NavbarAlign[];
export const NAVBAR_COLLAPSE = ["responsive", "expanded", "collapsed"] as const satisfies readonly NavbarCollapse[];
export interface NavbarArrangements { rail: boolean; compact: boolean }
/** Decide arrangements without consulting viewport state in JavaScript. */
export function arrangements(collapse: NavbarCollapse): NavbarArrangements { return { rail: collapse !== "collapsed", compact: collapse !== "expanded" }; }
export interface NavbarLink {
  id: string;
  label: string;
  href?: string;
  current?: boolean | "page" | "step" | "location" | "date" | "time" | "true";
  active?: boolean;
  disabled?: boolean;
  onselect?: () => void;
}
