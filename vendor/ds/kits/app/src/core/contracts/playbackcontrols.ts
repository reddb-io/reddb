export interface PlaybackControlsContract {
  /** Accessible name of the replay controls. */
  label: string;
  /** Caller-owned running state; this component never starts a timer. */
  playing: boolean;
  /** Caller-owned position in the same unit as duration. */
  position: number;
  /** End of the replay range. */
  duration: number;
  /** Current playback multiplier. */
  speed?: number;
  /** Reports play/pause intent; omitted, the action is disabled. */
  onplayingchange?: (playing: boolean) => void;
  /** Reports a requested position; omitted, seeking is disabled. */
  onpositionchange?: (position: number) => void;
  /** Reports a speed request; omitted, speed selection is disabled. */
  onspeedchange?: (speed: number) => void;
  /** Requests one next step; omitted, the action is disabled. */
  onstep?: () => void;
}
