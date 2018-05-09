use field::FieldIdent;
use apply::{Map, Apply, ApplyTo, MapFn};
use error::*;
use field::DataType;
use masked::MaybeNa;

/// Trait implemented by data structures that represent a single column / vector / field of data.
pub trait DataIndex<T: DataType> {
    /// Returns the data (possibly NA) at the specified index, if it exists.
    fn get_data(&self, idx: usize) -> Result<MaybeNa<&T>>;
    /// Returns the length of this data field.
    fn len(&self) -> usize;
}

impl<'a, T: DataType> DataIndex<T> for Vec<MaybeNa<&'a T>> {
    fn get_data(&self, idx: usize) -> Result<MaybeNa<&T>> {
        Ok(self[idx].clone())
    }
    fn len(&self) -> usize {
        self.len()
    }
}
impl<T: DataType> DataIndex<T> for Vec<MaybeNa<T>> {
    fn get_data(&self, idx: usize) -> Result<MaybeNa<&T>> {
        Ok(self[idx].as_ref())
    }
    fn len(&self) -> usize {
        self.len()
    }
}
impl<T: DataType> DataIndex<T> for Vec<T> {
    fn get_data(&self, idx: usize) -> Result<MaybeNa<&T>> {
        Ok(MaybeNa::Exists(&self[idx]))
    }
    fn len(&self) -> usize {
        self.len()
    }
}

/// Either an owned data structure or reference to a data structure that implements `DataIndex`.
pub enum OwnedOrRef<'a, T: 'a + DataType> {
    /// A boxed data structure that implemented `DataIndex`.
    Owned(Box<DataIndex<T> + 'a>),
    /// A reference to a data structure that implements `DataIndex`.
    Ref(&'a DataIndex<T>)
}
impl<'a, T: 'a + DataType> OwnedOrRef<'a, T> {
    /// Returns a reference to the underlying `DataIndex`, whether this `OwnedOrRef` owns the data
    /// or simply possesses a reference to it.
    pub fn as_ref(&'a self) -> &'a DataIndex<T> {
        match *self {
            OwnedOrRef::Owned(ref data) => data.as_ref(),
            OwnedOrRef::Ref(data) => data,
        }
    }
}
impl<'a, T: 'a + DataType> DataIndex<T> for OwnedOrRef<'a, T> {
    fn get_data(&self, idx: usize) -> Result<MaybeNa<&T>> {
        match *self {
            OwnedOrRef::Owned(ref data) => data.get_data(idx),
            OwnedOrRef::Ref(ref data) => data.get_data(idx),
        }
    }
    fn len(&self) -> usize {
        match *self {
            OwnedOrRef::Owned(ref data) => data.len(),
            OwnedOrRef::Ref(ref data) => data.len(),
        }
    }
}

/// A generic structure to hold either an owned or reference structure which implements `DataIndex`,
/// of any of the accepted agnes types.
pub enum ReduceDataIndex<'a> {
    /// An unsigned data structure implementing `DataIndex`.
    Unsigned(OwnedOrRef<'a, u64>),
    /// An signed data structure implementing `DataIndex`.
    Signed(OwnedOrRef<'a, i64>),
    /// An text data structure implementing `DataIndex`.
    Text(OwnedOrRef<'a, String>),
    /// An boolean data structure implementing `DataIndex`.
    Boolean(OwnedOrRef<'a, bool>),
    /// An floating-point data structure implementing `DataIndex`.
    Float(OwnedOrRef<'a, f64>),
}

/// Type for accessing a specified field (identified by a `FieldIdent`) for an underlying data
/// structure.
#[derive(Debug, Clone)]
pub struct Selection<'a, 'b, D: 'a + ?Sized> {
    /// Underlying data structure for this selection. Contains the field identified by `ident`.
    pub data: &'a D,
    /// Identifier of the field within the `data` structure.
    pub ident: &'b FieldIdent,
}

impl<'a, 'b, D: 'a + ApplyTo> Apply for Selection<'a, 'b, D> {
    fn apply<F: MapFn>(&self, f: &mut F) -> Result<Vec<F::Output>> {
        self.data.apply_to(f, &self.ident)
    }
}

impl<'a, 'b, D> Selection<'a, 'b, D> {
    /// Create a new `Selection` object from specified data and identifier.
    pub fn new(data: &'a D, ident: &'b FieldIdent) -> Selection<'a, 'b, D> {
        Selection {
            data,
            ident: ident
        }
    }
}
impl<'a, 'b, D: ApplyTo> Selection<'a, 'b, D> {
    /// Apply a `MapFn` to this selection (to be lazy evaluated).
    pub fn map<F: MapFn>(&self, f: F) -> Map<Self, F> {
        Map::new(self, f, None)
    }
}

/// Trait for types that can have a specific field selected (for applying `MapFn`s).
pub trait Select {
    /// Select the specified field.
    fn select<'a, 'b>(&'a self, ident: &'b FieldIdent) -> Selection<'a, 'b, Self>;
}

impl<T> Select for T {
    fn select<'a, 'b>(&'a self, ident: &'b FieldIdent)
        -> Selection<'a, 'b, Self>
    {
        Selection::new(self, ident)
    }
}

/// Data selector for the `ApplyToElem` and `ApplyToField` methods.
pub trait Selector: Clone {
    /// The type of the selector (the information used to specify what the `FieldFn` or `MapFn`
    /// operates upon).
    type IndexType;
    /// Returns the field / element selector details.
    fn index(&self) -> Self::IndexType;
}
/// A data selector unsing only a data index. Used to select a specific element among a
/// single column / field / vector for use with an `MapFn`.
#[derive(Debug, Clone)]
pub struct IndexSelector(pub usize);
impl Selector for IndexSelector {
    type IndexType = usize;
    fn index(&self) -> usize { self.0 }
}
/// A data selector using both a data field identifier and the data index. Used to select a
/// specific element in a two-dimensional data structs (with both fields and elements) along with
/// a `FieldFn`.
#[derive(Debug, Clone)]
pub struct FieldIndexSelector<'a>(pub &'a FieldIdent, pub usize);
impl<'a> Selector for FieldIndexSelector<'a> {
    type IndexType = (&'a FieldIdent, usize);
    fn index(&self) -> (&'a FieldIdent, usize) { (self.0, self.1) }
}
/// A data selector using only a field identifier. Used to select a specific field to be passed to
/// `FieldFn`.
#[derive(Debug, Clone)]
pub struct FieldSelector<'a>(pub &'a FieldIdent);
impl<'a> Selector for FieldSelector<'a> {
    type IndexType = (&'a FieldIdent);
    fn index(&self) -> (&'a FieldIdent) { (self.0) }
}
/// A data selector with no data. Used to select an entire field with `FieldFn` when a data
/// structure only has a single field's data.
#[derive(Debug, Clone)]
pub struct NilSelector;
impl Selector for NilSelector {
    type IndexType = ();
    fn index(&self) -> () {}
}
