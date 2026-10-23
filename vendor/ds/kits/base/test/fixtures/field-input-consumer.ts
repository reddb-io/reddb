// Field and Input through the same Base subpath a Product Application uses.
//
// Keeping this fixture at the package boundary means the behavior suites fail
// if either implementation exists privately without becoming a public Base
// capability, or if an Extension Seam is present but cannot be imported.

export {
  Field,
  Input,
  field,
  input,
  type FieldControlProps,
  type FieldVariants,
  type InputVariants,
} from "@reddb-io/design-system/base";
