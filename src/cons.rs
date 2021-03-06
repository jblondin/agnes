/*!
Basic heterogeneous list ([cons-list](https://en.wikipedia.org/wiki/Cons#Lists)) implementation.
*/

use std::ops::Add;

use typenum::{Add1, UTerm, Unsigned, B1};

/// The end of a heterogeneous type list.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct Nil;
/// Buildling block of a heterogeneous type list.
#[derive(Debug, Clone, Hash, PartialEq, Eq)]
pub struct Cons<H, T> {
    /// Value of this element of the type list.
    pub head: H,
    /// Remaining elements of the type list.
    pub tail: T,
}

/// Utility function to construct a [Cons](struct.Cons.html) list.
pub fn cons<H, T>(head: H, tail: T) -> Cons<H, T> {
    Cons { head, tail }
}

/// Trait for adding a new element to the front of a [heterogeneous list](struct.Cons.html).
pub trait PushFront<H> {
    /// The resulting cons-list type after pushing the specified `H` element to the front of the
    /// existing list.
    type Output;

    /// Add an element to the front of this heterogeneous list.
    fn push_front(self, head: H) -> Self::Output;
}

impl<NewH, H, T> PushFront<NewH> for Cons<H, T> {
    type Output = Cons<NewH, Self>;

    fn push_front(self, head: NewH) -> Self::Output {
        cons(head, self)
    }
}
impl<NewH> PushFront<NewH> for Nil {
    type Output = Cons<NewH, Nil>;

    fn push_front(self, head: NewH) -> Self::Output {
        cons(head, self)
    }
}

/// Trait for adding a new element to the end of a [heterogeneous list](struct.Cons.html).
pub trait PushBack<U> {
    /// The resulting cons-list type after pushing the specified `H` element to the front of the
    /// existing list.
    type Output;

    /// Add an element to the end of this heterogeneous list.
    fn push_back(self, elem: U) -> Self::Output;
}
impl<U> PushBack<U> for Nil {
    type Output = Cons<U, Nil>;
    fn push_back(self, elem: U) -> Cons<U, Nil> {
        cons(elem, Nil)
    }
}
impl<U, H, T> PushBack<U> for Cons<H, T>
where
    T: PushBack<U>,
{
    type Output = Cons<H, T::Output>;
    fn push_back(self, elem: U) -> Cons<H, T::Output> {
        cons(self.head, self.tail.push_back(elem))
    }
}

/// Trait for adding a list to the end of a [heterogeneous list](struct.Cons.html).
pub trait Append<List> {
    /// The resulting cons-list type after adding the element of the target list to the end of the
    /// existing list.
    type Appended;
    /// Add a list to the end of this heterogeneous list.
    fn append(self, list: List) -> Self::Appended;
}
impl<List> Append<List> for Nil {
    type Appended = List;
    fn append(self, list: List) -> List {
        list
    }
}
impl<List, H, T> Append<List> for Cons<H, T>
where
    T: Append<List>,
{
    type Appended = Cons<H, T::Appended>;
    fn append(self, list: List) -> Cons<H, T::Appended> {
        cons(self.head, self.tail.append(list))
    }
}

// TODO: idea for macro framework for applying function to each value in a cons-list
//
// list_apply![
//     self.frames; // list to apply this to
//     |order: &[usize]| { /* closure to apply for recursive case */
//         head.update_permutation(order);
//         tail.update_permutation(order);
//     }
//     |order: &[usize]| {} /* base-case closure */
// ]

/// Trait providing length (either compile-time or runtime) details of a list.
pub trait Len {
    /// `typenum`-based list length.
    type Len: Unsigned;

    /// Returns `true` if length is 0, and `false` otherwise.
    fn is_empty() -> bool {
        Self::len() == 0
    }

    /// Returns the length of this list.
    fn len() -> usize {
        Self::Len::to_usize()
    }
}

impl Len for Nil {
    type Len = UTerm;
}
impl<Head, Tail> Len for Cons<Head, Tail>
where
    Tail: Len,
    <Tail as Len>::Len: Add<B1>,
    <<Tail as Len>::Len as Add<B1>>::Output: Unsigned,
{
    type Len = Add1<<Tail as Len>::Len>;
}

/// Utility macro to determine the length of a cons-list.
#[macro_export]
macro_rules! length {
    ($list:ty) => {
        <<$list as $crate::cons::Len>::Len as typenum::Unsigned>::USIZE
    };
}
