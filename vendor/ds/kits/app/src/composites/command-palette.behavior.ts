/** One caller-owned command offered by the global command search. */
export interface CommandPaletteCommand {
  /** Stable identity returned when the command is selected. */
  id: string;
  /** Visible, searchable command name. */
  label: string;
  /** Removes only this command from interaction and selection. */
  disabled?: boolean;
  /** Caller-owned business action. */
  onselect?: () => void;
}
