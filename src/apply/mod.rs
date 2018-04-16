/*!
Framework for providing and applying functions to data within the `agnes` data structures in a
consistent, type-coherent manner.

The `ElemFn` trait provides a framework for functions that apply to a single element in the data
structure.

The `FieldFn` trait provides a framework for functions that apply to a field (column) of data in
the data structure.
*/

mod selector;
pub use self::selector::*;

mod elem_fn;
pub use self::elem_fn::*;

mod field_fn;
pub use self::field_fn::*;

mod matches;
pub use self::matches::*;

mod sort_order;
pub use self::sort_order::*;