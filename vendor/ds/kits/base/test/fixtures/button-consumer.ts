// The Base subpath as a Product Application consumes it.
//
// Keeping this import at the package boundary matters: importing the component
// file directly would let the implementation exist without proving that the
// universal public contract is actually reachable from `./base`.

export {
  BUTTON_INTENTS,
  BUTTON_TONES,
  TONES,
  BUTTON_SIZES,
  BUTTON_VARIANTS,
  Button,
  button,
  buttonSpinner,
} from "@reddb-io/design-system/base";
