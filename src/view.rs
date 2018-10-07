/*!
Main `DataView` struct and associated implementations.

# Aggregation

There are three types of data aggregation supported by `agnes`:
* Data merging -- combining two `DataView` objects with the same number of records together,
creating a new `DataView` with all the fields of the two source `DataView`s.
* Data appending -- combining two `DataView` objects with the same fields, creating a new `DataView`
object with all of the records of the two source `DataView`s.
* Data joining -- combining two `DataView` objects using specified join, creating a new
`DataView` object with a subset of records from the two source `DataView`s according to the join
parameters.

*/
use std::hash::{Hash, Hasher};
use std::fmt::{self, Display, Formatter};
use std::iter::FromIterator;
use std::collections::HashSet;

use indexmap::IndexMap;
use indexmap::map::Keys;
use serde::ser::{self, Serialize, Serializer, SerializeMap};
use prettytable as pt;

use access::DataIndex;
use frame::{DataFrame, FramedMap, FramedTMap, FramedMapExt, Framed, Filter, FramedFunc,
    SerializedField, Reindexer};
use field::{Value};
use join::{Join, sort_merge_join, compute_merged_frames, compute_merged_field_list};
use field::{FieldIdent, RFieldIdent};
use error;
use store::{DataStore, CopyInto, StoreRecord};
use data_types::*;
use apply::sort::{DtOrd, SortOrderFunc};
// use apply::mapfn::*;
use select::{SelectField, Field};

/// A field in a `DataView`. Contains the (possibly-renamed) field identifier and the store index
/// with the underlying data.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ViewField {
    /// The field identifier, along with renaming information (if exists)
    pub rident: RFieldIdent,
    /// Frame index of the underlying data
    pub frame_idx: usize,
}

/// A 'view' into a data store. The primary struct for viewing and manipulating data.
#[derive(Debug, Clone, Default)]
pub struct DataView<DTypes>
    where DTypes: DTypeList
{
    pub(crate) frames: Vec<DataFrame<DTypes>>,
    pub(crate) fields: IndexMap<FieldIdent, ViewField>,
}

impl<DTypes> DataView<DTypes>
    where DTypes: DTypeList
{
    /// Generate a new subview of this DataView.
    pub fn v<L: IntoFieldList>(&self, s: L) -> DataView<DTypes> {
        let mut sub_fields = IndexMap::new();
        for ident in s.into_field_list().iter() {
            if let Some(field) = self.fields.get(ident) {
                sub_fields.insert(ident.clone(), field.clone());
            }
        }
        DataView {
            frames: self.frames.clone(),
            fields: sub_fields,
        }
    }
    /// Generate a new subview of this DataView, generating an error if a specified field does
    /// not exist.
    pub fn subview<L: IntoFieldList>(&self, s: L) -> error::Result<DataView<DTypes>> {
        let mut sub_fields = IndexMap::new();
        for ident in s.into_field_list().iter() {
            if let Some(field) = self.fields.get(ident) {
                sub_fields.insert(ident.clone(), field.clone());
            } else {
                return Err(error::AgnesError::FieldNotFound(ident.clone()));
            }
        }
        Ok(DataView {
            frames: self.frames.clone(),
            fields: sub_fields,
        })
    }
    /// Number of rows in this data view
    pub fn nrows(&self) -> usize
        where DTypes::Storage: MaxLen<DTypes>
    {
        if self.frames.len() == 0 { 0 } else { self.frames[0].nrows() }
    }
    /// Returns `true` if the DataView is empty (has no rows or has no fields)
    pub fn is_empty(&self) -> bool
        where DTypes::Storage: MaxLen<DTypes>
    {
        self.nrows() == 0
    }
    /// Number of fields in this data view
    pub fn nfields(&self) -> usize {
        self.fields.len()
    }
    /// Field names in this data view
    pub fn fieldnames(&self) -> Vec<&FieldIdent> {
        self.fields.keys().collect()
    }
    /// Return the field type for specified field
    pub(crate) fn get_field_type(&self, ident: &FieldIdent) -> Option<DTypes::DType> {
        self.fields.get(ident).and_then(|view_field: &ViewField| {
            self.frames[view_field.frame_idx].get_field_type(&view_field.rident.ident)
        })
    }
    // /// Return the field types (as `TypedFieldIdent` structs) of this DataView
    // pub fn field_types(&self) -> Vec<TypedFieldIdent> {
    //     self.fields.iter().map(|(ident, vf)| {
    //         TypedFieldIdent {
    //             ident: ident.clone(),
    //             // since we're iterating over fields we know exist, unwrap is safe
    //             ty: self.frames[vf.frame_idx].get_field_type(&vf.rident.ident).unwrap()
    //         }
    //     }).collect()
    // }
    /// Returns `true` if this `DataView` contains this field.
    pub fn has_field(&self, s: &FieldIdent) -> bool {
        self.fields.contains_key(s)
    }

    /// Rename a field of this DataView.
    pub fn rename<T, U>(&mut self, orig: T, new: U) -> error::Result<()> where
        T: Into<FieldIdent>,
        U: Into<FieldIdent>
    {
        let (orig, new) = (orig.into(), new.into());
        if self.fields.contains_key(&new) {
            return Err(error::AgnesError::FieldCollision(vec![new]));
        }
        let new_vf = if let Some(ref orig_vf) = self.fields.get(&orig) {
            ViewField {
                rident: RFieldIdent {
                    ident: orig_vf.rident.ident.clone(),
                    rename: Some(new.to_string())
                },
                frame_idx: orig_vf.frame_idx,
            }
        } else {
            return Err(error::AgnesError::FieldNotFound(orig));
        };
        self.fields.insert(new_vf.rident.to_renamed_field_ident(), new_vf);
        self.fields.swap_remove(&orig);
        Ok(())
    }

    /// Merge this `DataView` with another `DataView` object, creating a new `DataView` with the
    /// same number of rows and all the fields from both source `DataView` objects.
    pub fn merge(&self, other: &DataView<DTypes>) -> error::Result<DataView<DTypes>>
        where DTypes::Storage: MaxLen<DTypes>
    {
        if self.nrows() != other.nrows() {
            return Err(error::AgnesError::DimensionMismatch(
                "number of rows mismatch in merge".into()));
        }

        // compute merged stores (and mapping from 'other' store index references to combined
        // store vector)
        let (new_frames, other_store_indices) = compute_merged_frames(self, other);

        // compute merged field list
        let (_, mut new_fields) =
            compute_merged_field_list(self, other, &other_store_indices, None)?;
        let new_fields = IndexMap::from_iter(new_fields.drain(..));
        Ok(DataView {
            frames: new_frames,
            fields: new_fields
        })
    }

    /// Combine two `DataView` objects using specified join, creating a new `DataStore` object with
    /// a subset of records from the two source `DataView`s according to the join parameters.
    ///
    /// Note that since this is creating a new `DataStore` object, it will be allocated new data to
    /// store the contents of the joined `DataView`s.
    pub fn join<'b, T>(&'b self, other: &'b DataView<DTypes>, join: Join)
        -> error::Result<DataStore<DTypes>>
        where T: 'static + DataType<DTypes> + DtOrd + PartialEq + Default,
              // for<'a> Framed<'a, DTypes, T>: SortOrder<T, Framed<'a, DTypes, T>>,
              DTypes: 'b,
              DTypes::Storage: MaxLen<DTypes> + TypeSelector<DTypes, T> + CreateStorage
                      + for<'c> FramedMapExt<DTypes, CopyInto<'c, DTypes>, ()>

              // DTypes: 'b + MaxLen<DTypes> + TypeSelector<T, Idx> + AssociatedValue<'a>
                //+ MapForTypeNum<DTypes, CopyInto<'a, 'b, DTypes>>
    {
        match join.predicate {
            // TODO: implement hash join
            // Predicate::Equal => {
            //     hash_join(self, other, join)
            // },
            _ => {
                sort_merge_join::<DTypes, T>(self, other, join)

            }
        }
    }

    // /// Returns an iterator over `Selection`s (the result of calling `select` on a DataView) of all
    // /// the fields in this DataView.
    // pub fn selections<'a>(&'a self) -> Selections<'a, DTypes> {
    //     Selections::new(self, self.fields.keys())
    // }

    /// Returns an iterator over the fields (as `FieldIdent`s of this DataView.
    pub fn fields<'a>(&'a self) -> Keys<'a, FieldIdent, ViewField> {
        self.fields.keys()
    }

    // pub fn field<'a, T: 'a + DataType, I: Into<FieldIdent>>(&'a self, ident: I)
    //     -> error::Result<Framed<'a, T>>
    //     where Self: Field_<'a, T, Output=Framed<'a, T>>
    // {
    //     (self as &dyn Field_<'a, T, Output=Framed<'a, T>>).field(ident.into())
    // }

    pub fn map<F, FOut, I>(&self, ident: I, f: F)
        -> error::Result<FOut>
        where DTypes::Storage: FramedMap<DTypes, F, FOut>,
              I: Into<FieldIdent>
    {
        let ident = ident.into();
        self.fields.get(&ident)
            .ok_or(error::AgnesError::FieldNotFound(ident))
            .and_then(|view_field: &ViewField| {
                self.frames[view_field.frame_idx].map(&view_field.rident.ident, f)
            })
    }
    pub fn tmap<T, F, I>(&self, ident: I, f: F)
        -> error::Result<F::Output>
        where F: Func<DTypes, T>,
              T: DataType<DTypes>,
              DTypes::Storage: MaxLen<DTypes> + FramedTMap<DTypes, T, F>,
              I: Into<FieldIdent>,
    {
        let ident = ident.into();
        self.fields.get(&ident)
            .ok_or(error::AgnesError::FieldNotFound(ident))
            .and_then(|view_field: &ViewField| {
                self.frames[view_field.frame_idx].tmap(&view_field.rident.ident, f)
            })
    }

    pub fn map_ext<F, FOut, I>(&self, ident: I, f: F)
        -> error::Result<FOut>
        where DTypes::Storage: FramedMapExt<DTypes, F, FOut>,
              I: Into<FieldIdent>
    {
        let ident = ident.into();
        self.fields.get(&ident)
            .ok_or(error::AgnesError::FieldNotFound(ident))
            .and_then(|view_field: &ViewField| {
                self.frames[view_field.frame_idx].map_ext(&view_field.rident.ident, f)
            })
    }
    pub fn map_partial<F, I>(&self, ident: I, f: F)
        -> error::Result<Option<F::Output>>
        where DTypes::Storage: MapPartial<DTypes, F> + MaxLen<DTypes>,
              F: FuncPartial<DTypes>,
              I: Into<FieldIdent>
    {
        let ident = ident.into();
        self.fields.get(&ident)
            .ok_or(error::AgnesError::FieldNotFound(ident))
            .and_then(|view_field: &ViewField| {
                self.frames[view_field.frame_idx].map_partial(&view_field.rident.ident, f)
            })
    }
    // pub fn map_opt<'a, F, FOut, Flag>(&'a self, ident: &FieldIdent, f: F)
    //     -> error::Result<Option<FOut>>
    //     where DTypes::Storage: MapOpt<FramedFunc<'a, DTypes, F>, FOut, Flag>,
    // {
    //     self.fields.get(ident)
    //         .ok_or(error::AgnesError::FieldNotFound(ident.clone()))
    //         .and_then(|view_field: &ViewField| {
    //             self.frames[view_field.frame_idx].map_opt(&view_field.rident.ident, f)
    //         })
    // }

    // pub fn copy_into(
    //     &self,
    //     ident: &FieldIdent,
    //     idx: usize,
    //     target_ds: &mut DataStore<DTypes>,
    //     target_ident: &FieldIdent,
    // )
    //     -> error::Result<()>
    //     where DTypes: TypeNumMapInto<CopyInto, ()> + TypeNumAddVec

    //     // where DTypes: MapForTypeNum<DTypes, CopyInto<'a, 'b, DTypes>>
    // {
    //     // println!("from:{} into:{}", ident, target_ident);
    //     self.fields.get(ident)
    //         .ok_or(error::AgnesError::FieldNotFound(ident.clone()))
    //         .and_then(move |view_field: &ViewField| {
    //             self.frames[view_field.frame_idx].copy_into(
    //                 &view_field.rident.ident, // use original ident
    //                 idx,
    //                 target_ds,
    //                 target_ident
    //             )
    //         })
    // }

    fn sort_by<'a>(&'a mut self, ident: &FieldIdent) -> error::Result<Vec<usize>>
        where DTypes::Storage: FramedMap<DTypes, SortOrderFunc, Vec<usize>>,
    {
        match self.fields.get(ident) {
            Some(view_field) => {
                // filter on frame index this field belongs to
                let sorted = self.frames[view_field.frame_idx].sort_by(&view_field.rident.ident)?;
                // apply same filter to rest of frames
                for frame_idx in 0..self.frames.len() {
                    if frame_idx != view_field.frame_idx {
                        self.frames[frame_idx].update_permutation(&sorted);
                    }
                }
                Ok(sorted)
            },
            None => Err(error::AgnesError::FieldNotFound(ident.clone()))
        }
    }
}

impl<'a, DTypes> DataView<DTypes>
    where DTypes: DTypeList + RefAssocTypes<'a>
{
    // pub fn record<'a>(&'a self, idx: usize)
    //     -> Record<'a, DTypes>
    //     where DTypes: AssocTypes + RefAssocTypes<'a>,
    //           DTypes::RecordValues: RetrieveValues<'a, DTypes::Storage>,
    // {

    // }
    pub fn partial_record<I, Iter, IntoIter>(&'a self, idx: usize, idents: IntoIter)
       -> error::Result<PartialRecord<'a, DTypes>>
        where DTypes: AssocTypes + RefAssocTypes<'a>,
              DTypes::PartialRecordValues: RetrieveValuesPartial<'a, DTypes, DTypes::Storage>,
              DataFrame<DTypes>: Reindexer<DTypes>,
              I: Into<FieldIdent>,
              Iter: Iterator<Item=I> + Clone,
              IntoIter: IntoIterator<Item=I, IntoIter=Iter>,
    {
        let idents_iter = idents.into_iter();
        let frame_idx_sets = idents_iter.clone()
                .map(|ident| {
                    let ident = ident.into();
                    self.fields.get(&ident).ok_or(error::AgnesError::FieldNotFound(ident))
                })
                // intermediate collect to check for missing fields
                .collect::<error::Result<Vec<_>>>()?.iter()
                .map(|view_field: &&ViewField| view_field.frame_idx)
              .collect::<HashSet<_>>();
        Ok(PartialRecord {
            store_records: frame_idx_sets
                .iter()
                .map(|&frame_idx| self.frames[frame_idx].store_record(idx, idents_iter.clone()))
                .collect()
        })
    }
}


#[derive(Debug, Clone)]
pub struct PartialRecord<'a, DTypes>
    where DTypes: DTypeList + RefAssocTypes<'a>
{
    store_records: Vec<StoreRecord<'a, DTypes>>,
}
// impl<'a, DTypes> Hash for PartialRecord<'a, DTypes>
//     where DTypes: DTypeList + RefAssocTypes<'a>
// {
//     fn hash<H>(&self, state: &mut H) where H: Hasher {
//         self.store_records.hash(state);
//     }
// }
// impl<'a, DTypes> PartialEq for PartialRecord<'a, DTypes>
//     where DTypes: DTypeList + RefAssocTypes<'a>
// {
//     fn eq(&self, other: &PartialRecord<'a, DTypes>) -> bool {
//         self.store_records.iter().eq(other.store_records.iter())
//     }
// }
// impl<'a, DTypes> Eq for PartialRecord<'a, DTypes>
//     where DTypes: DTypeList + RefAssocTypes<'a>
// {}


impl<DTypes> Field<DTypes> for DataView<DTypes>
    where DTypes: DTypeList
{}

impl<'a, DTypes, T> SelectField<'a, T, DTypes>
    for DataView<DTypes>
    where T: 'static + DataType<DTypes>,
          DTypes: 'a + DTypeList,
          DTypes::Storage: MaxLen<DTypes>
{
    type Output = Framed<'a, DTypes, T>;

    fn select(&'a self, ident: FieldIdent) -> error::Result<Framed<'a, DTypes, T>>
        where DTypes: AssocTypes,
              DTypes::Storage: TypeSelector<DTypes, T>
    {
        // let ident = ident.into();
        self.fields.get(&ident)
            .ok_or(error::AgnesError::FieldNotFound(ident.clone()))
            .and_then(|view_field: &ViewField| {
                self.frames[view_field.frame_idx].select(view_field.rident.ident.clone())
            })
    }
}

// /// An iterator over `Selection`s of fields in a DataView.
// pub struct Selections<'a, DTypes>
//     where DTypes: 'a + DTypeList
// {
//     dv: &'a DataView<DTypes>,
//     keys: Keys<'a, FieldIdent, ViewField>
// }
// impl<'a, DTypes> Selections<'a, DTypes>
//     where DTypes: DTypeList
// {
//     fn new(dv: &'a DataView<DTypes>, keys: Keys<'a, FieldIdent, ViewField>)
//         -> Selections<'a, DTypes>
//     {
//         Selections {
//             dv,
//             keys
//         }
//     }
// }
// impl<'a, DTypes> Iterator for Selections<'a, DTypes>
//     where DTypes: DTypeList
// {
//     type Item = Selection<DataView<DTypes>>;
//     fn next(&mut self) -> Option<Selection<DataView<DTypes>>> {
//         self.keys.next().map(|ident| Selection::new(self.dv, ident.clone()))
//     }
// }

// impl ApplyTo for DataView {
//     fn apply_to<F: MapFn>(&self, f: &mut F, ident: &FieldIdent)
//         -> error::Result<Vec<F::Output>>
//     {
//         self.fields.get(ident)
//             .ok_or(error::AgnesError::FieldNotFound(ident.clone()))
//             .and_then(|view_field: &ViewField| {
//                 self.frames[view_field.frame_idx].apply_to(f, &view_field.rident.ident)
//             }
//         )
//     }
// }
// impl ApplyToElem for DataView {
//     fn apply_to_elem<F: MapFn>(&self, f: &mut F, ident: &FieldIdent, idx: usize)
//         -> error::Result<F::Output>
//     {
//         self.fields.get(ident)
//             .ok_or(error::AgnesError::FieldNotFound(ident.clone()))
//             .and_then(|view_field: &ViewField| {
//                 self.frames[view_field.frame_idx].apply_to_elem(f, &view_field.rident.ident, idx)
//             }
//         )
//     }
// }
// impl FieldApplyTo for DataView {
//     fn field_apply_to<F: FieldMapFn>(&self, f: &mut F, ident: &FieldIdent)
//         -> error::Result<F::Output>
//     {
//         self.fields.get(ident)
//             .ok_or(error::AgnesError::FieldNotFound(ident.clone()))
//             .and_then(|view_field: &ViewField| {
//                 self.frames[view_field.frame_idx].field_apply_to(f, &view_field.rident.ident)
//             }
//         )
//     }
// }

// impl<'a> ApplyFieldReduce<'a> for Selection<'a, DataView> {
//     fn apply_field_reduce<F: FieldReduceFn<'a>>(&self, f: &mut F)
//         -> error::Result<F::Output>
//     {
//         self.data.fields.get(&self.ident)
//             .ok_or(error::AgnesError::FieldNotFound(self.ident.clone()))
//             .and_then(|view_field: &ViewField| {
//                 self.data.frames[view_field.frame_idx].select_one(&view_field.rident.ident)
//                     .apply_field_reduce(f)
//             }
//         )
//     }
// }
// impl<'a> ApplyFieldReduce<'a> for Vec<Selection<'a, DataView>> {
//     fn apply_field_reduce<F: FieldReduceFn<'a>>(&self, f: &mut F)
//         -> error::Result<F::Output>
//     {
//         self.iter().map(|selection| {
//             selection.data.fields.get(&selection.ident)
//                 .ok_or(error::AgnesError::FieldNotFound(selection.ident.clone()))
//                 .map(|view_field: &ViewField| {
//                     selection.data.frames[view_field.frame_idx].select_one(&view_field.rident.ident)
//                 })
//         }).collect::<error::Result<Vec<_>>>()
//             .and_then(|frame_selections| frame_selections.apply_field_reduce(f))
//     }
// }

impl<DTypes, T> Filter<DTypes, T> for DataView<DTypes>
    where DataFrame<DTypes>: Filter<DTypes, T> + Field<DTypes>,
          DTypes: DTypeList,
          DTypes::Storage: MaxLen<DTypes> + TypeSelector<DTypes, T>,
          T: 'static + DataType<DTypes>
{
    fn filter<I: Into<FieldIdent>, F: Fn(&T) -> bool>(&mut self, ident: I, pred: F)
        -> error::Result<Vec<usize>>
    {
        let ident = ident.into();
        match self.fields.get(&ident) {
            Some(view_field) => {
                // filter on frame index this field belongs to
                let filter = self.frames[view_field.frame_idx].filter(
                    &view_field.rident.ident, pred)?;
                // apply same filter to rest of frames
                for frame_idx in 0..self.frames.len() {
                    if frame_idx != view_field.frame_idx {
                        self.frames[frame_idx].update_permutation(&filter);
                    }
                }
                Ok(filter)
            },
            None => Err(error::AgnesError::FieldNotFound(ident.clone()))
        }
    }
}

impl<DTypes> From<DataStore<DTypes>> for DataView<DTypes>
    where DTypes: DTypeList
{
    fn from(store: DataStore<DTypes>) -> DataView<DTypes> {
        let mut fields = IndexMap::new();
        for ident in store.fields() {
            fields.insert(ident.clone(), ViewField {
                rident: RFieldIdent {
                    ident: ident.clone(),
                    rename: None
                },
                frame_idx: 0,
            });
        }
        DataView {
            frames: vec![store.into()],
            fields: fields
        }
    }
}

const MAX_DISP_ROWS: usize = 1000;
impl<DTypes> Display for DataView<DTypes>
    where DTypes: DTypeList,
          DTypes::Storage: MaxLen<DTypes>
                  + for<'a, 'b> Map<DTypes, FramedFunc<'a, DTypes, AddCellToRow<'b>>, ()>
{
    fn fmt(&self, f: &mut Formatter) -> Result<(), fmt::Error> {
        if self.frames.len() == 0 || self.fields.len() == 0 {
            return write!(f, "Empty DataView");
        }
        let nrows = self.frames[0].nrows();

        let mut table = pt::Table::new();
        table.set_titles(self.fields.keys().into());
        let mut rows = vec![pt::row::Row::empty(); nrows.min(MAX_DISP_ROWS)];
        for view_field in self.fields.values() {
            match self.frames[view_field.frame_idx].map(
                &view_field.rident.ident,
                AddCellToRow {
                    rows: &mut rows,
                },
            ) {
                Ok(_) => {},
                Err(e) => { return write!(f, "view display error: {}", e); },
            }
        }
        // for field in self.fields.values() {
        //     // field.ajdfisofj();
        //     match self.apply_to(&mut AddCellToRow { rows: &mut rows, i: 0 },
        //         &field.rident.to_renamed_field_ident())
        //     {
        //         Ok(_) => {},
        //         Err(e) => { return write!(f, "view display error: {}", e); },
        //     };
        // }
        for row in rows.drain(..) {
            table.add_row(row);
        }
        table.set_format(*pt::format::consts::FORMAT_NO_BORDER_LINE_SEPARATOR);
        Display::fmt(&table, f)
    }
}

pub struct AddCellToRow<'a> {
    rows: &'a mut Vec<pt::row::Row>,
    // i: usize
}
impl<'a, DTypes, T> Func<DTypes, T> for AddCellToRow<'a>
    where DTypes: DTypeList,
          T: 'a + DataType<DTypes>,
          for<'b> Value<&'b T>: ToString
{
    type Output = ();
    fn call(
        &mut self,
        type_data: &dyn DataIndex<DTypes, DType=T>,
    )
    {
        for i in 0..type_data.len().min(MAX_DISP_ROWS) {
            self.rows[i].add_cell(cell!(type_data.get_datum(i).unwrap()));
        }
    }
}

// macro_rules! impl_apply_cell_to_row {
//     ($name:tt; $ty:ty) => {
//         fn $name(&mut self, value: Value<&$ty>) {
//             //FIXME: this will fail if i is > MAX_ROWS
//             // need to be able to short-circuit this once we hit MAX_ROWS
//             self.rows[self.i].add_cell(cell!(value));
//             self.i += 1;
//         }
//     }
// }
// impl<'a> MapFn for AddCellToRow<'a> {
//     type Output = ();
//     impl_apply_cell_to_row!(apply_unsigned; u64);
//     impl_apply_cell_to_row!(apply_signed;   i64);
//     impl_apply_cell_to_row!(apply_text;     String);
//     impl_apply_cell_to_row!(apply_boolean;  bool);
//     impl_apply_cell_to_row!(apply_float;    f64);
// }

impl<DTypes> Serialize for DataView<DTypes>
    where DTypes: DTypeList,
          DTypes::Storage: MaxLen<DTypes> + FieldSerialize<DTypes>
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error> where S: Serializer {
        let mut map = serializer.serialize_map(Some(self.fields.len()))?;
        for field in self.fields.values() {
            map.serialize_entry(&field.rident.to_string(), &SerializedField::new(
                field.rident.ident.clone(),
                &self.frames[field.frame_idx]
            ))?;
        }
        map.end()
    }
}

/// Marker trait to denote an object that serializes into a vector format
pub trait SerializeAsVec: Serialize {}
impl<T> SerializeAsVec for Vec<T> where T: Serialize {}

/// A 'view' into a single field's data in a data frame. This is a specialty view used to serialize
/// a `DataView` as a single sequence instead of as a map.
#[derive(Debug, Clone)]
pub struct FieldView<DTypes>
    where DTypes: DTypeList
{
    frame: DataFrame<DTypes>,
    field: RFieldIdent,
}

impl<DTypes> Serialize for FieldView<DTypes>
    where DTypes: DTypeList,
          DTypes::Storage: MaxLen<DTypes> + FieldSerialize<DTypes>,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where S: Serializer {
        if self.frame.has_field(&self.field.ident) {
            SerializedField::new(
                self.field.to_renamed_field_ident(),
                &self.frame
            ).serialize(serializer)
        } else {
            Err(ser::Error::custom(format!("missing field: {}", self.field.to_string())))
        }
    }
}
impl<DTypes> SerializeAsVec for FieldView<DTypes>
    where DTypes: DTypeList,
          DTypes::Storage: MaxLen<DTypes> + FieldSerialize<DTypes>
{}

impl<DTypes> DataView<DTypes>
    where DTypes: DTypeList
{
    /// Create a `FieldView` object from a `DataView` object, if possible. Typically, use this on
    /// `DataView` objects with only a single field; however, if the `DataView` object has multiple
    /// fields, the first one will be used for this `FieldView`. Returns `None` if the `DataView`
    /// has no fields (is empty).
    pub fn as_fieldview(&self) -> Option<FieldView<DTypes>> {
        if self.fields.is_empty() {
            None
        } else {
            // self.fields it not empty, so unwrap is safe
            let field = self.fields.values().next().unwrap();

            Some(FieldView {
                frame: self.frames[field.frame_idx].clone(),
                field: field.rident.clone(),
            })
        }
    }
}

/// Conversion trait for converting into a vector of FieldIdents. Used for indexing into a
/// `DataView`.
pub trait IntoFieldList {
    /// Convert into a `Vec<FieldIdents>`
    fn into_field_list(self) -> Vec<FieldIdent>;
}

impl IntoFieldList for FieldIdent {
    fn into_field_list(self) -> Vec<FieldIdent> {
        vec![self]
    }
}
impl<'a> IntoFieldList for &'a FieldIdent {
    fn into_field_list(self) -> Vec<FieldIdent> {
        vec![self.clone()]
    }
}
impl IntoFieldList for Vec<FieldIdent> {
    fn into_field_list(self) -> Vec<FieldIdent> {
        self
    }
}
impl<'a> IntoFieldList for Vec<&'a FieldIdent> {
    fn into_field_list(self) -> Vec<FieldIdent> {
        self.iter().map(|&fi| fi.clone()).collect()
    }
}

impl<'a> IntoFieldList for &'a str {
    fn into_field_list(self) -> Vec<FieldIdent> {
        vec![FieldIdent::Name(self.to_string())]
    }
}
impl<'a> IntoFieldList for Vec<&'a str> {
    fn into_field_list(mut self) -> Vec<FieldIdent> {
        self.drain(..).map(|s| FieldIdent::Name(s.to_string())).collect()
    }
}
macro_rules! impl_into_field_list_str_arr {
    ($val:expr) => {
        impl<'a> IntoFieldList for [&'a str; $val] {
            fn into_field_list(self) -> Vec<FieldIdent> {
                self.iter().map(|s| FieldIdent::Name(s.to_string())).collect()
            }
        }
    }
}
impl_into_field_list_str_arr!(1);
impl_into_field_list_str_arr!(2);
impl_into_field_list_str_arr!(3);
impl_into_field_list_str_arr!(4);
impl_into_field_list_str_arr!(5);
impl_into_field_list_str_arr!(6);
impl_into_field_list_str_arr!(7);
impl_into_field_list_str_arr!(8);
impl_into_field_list_str_arr!(9);
impl_into_field_list_str_arr!(10);
impl_into_field_list_str_arr!(11);
impl_into_field_list_str_arr!(12);
impl_into_field_list_str_arr!(13);
impl_into_field_list_str_arr!(14);
impl_into_field_list_str_arr!(15);
impl_into_field_list_str_arr!(16);
impl_into_field_list_str_arr!(17);
impl_into_field_list_str_arr!(18);
impl_into_field_list_str_arr!(19);
impl_into_field_list_str_arr!(20);


impl IntoFieldList for String {
    fn into_field_list(self) -> Vec<FieldIdent> {
        vec![FieldIdent::Name(self)]
    }
}
impl IntoFieldList for Vec<String> {
    fn into_field_list(mut self) -> Vec<FieldIdent> {
        self.drain(..).map(|s| FieldIdent::Name(s)).collect()
    }
}
macro_rules! impl_into_field_list_string_arr {
    ($val:expr) => {
        impl IntoFieldList for [String; $val] {
            fn into_field_list(self) -> Vec<FieldIdent> {
                // clone necessary since we're moving to the heap
                self.iter().map(|s| FieldIdent::Name(s.clone())).collect()
            }
        }
    }
}
impl_into_field_list_string_arr!(1);
impl_into_field_list_string_arr!(2);
impl_into_field_list_string_arr!(3);
impl_into_field_list_string_arr!(4);
impl_into_field_list_string_arr!(5);
impl_into_field_list_string_arr!(6);
impl_into_field_list_string_arr!(7);
impl_into_field_list_string_arr!(8);
impl_into_field_list_string_arr!(9);
impl_into_field_list_string_arr!(10);
impl_into_field_list_string_arr!(11);
impl_into_field_list_string_arr!(12);
impl_into_field_list_string_arr!(13);
impl_into_field_list_string_arr!(14);
impl_into_field_list_string_arr!(15);
impl_into_field_list_string_arr!(16);
impl_into_field_list_string_arr!(17);
impl_into_field_list_string_arr!(18);
impl_into_field_list_string_arr!(19);
impl_into_field_list_string_arr!(20);

#[cfg(test)]
mod tests {
    use test_utils::*;

    use data_types::HashableFieldCons;
    use super::FieldIdent;
    use error::*;
    use data_types::standard::*;
    use access::{DataIndex, DataIterator};

    use frame::{Filter};

    #[test]
    fn merge() {
        let ds1 = sample_emp_table();
        let ds2 = sample_emp_table_extra();

        let (dv1, dv2): (DataView, DataView) = (ds1.into(), ds2.into());
        println!("{}", dv1);
        println!("{}", dv2);
        let merged_dv: DataView = dv1.merge(&dv2).expect("merge failed");
        println!("{}", merged_dv);
        assert_eq!(merged_dv.nrows(), 7);
        assert_eq!(merged_dv.nfields(), 6);
        for (left, right) in merged_dv.fieldnames().iter()
            .zip(vec!["EmpId", "DeptId", "EmpName", "SalaryOffset", "DidTraining", "VacationHrs"]
                    .iter().map(|&s| FieldIdent::Name(s.into())))
        {
            assert_eq!(left, &&right);
        }
    }

    #[test]
    fn merge_dimension_mismatch() {
        let ds1 = sample_emp_table();
        let ds2 = sample_dept_table();

        let (dv1, dv2): (DataView, DataView) = (ds1.into(), ds2.into());
        println!("{}", dv1);
        println!("{}", dv2);
        match dv1.merge(&dv2) {
            Ok(_) => { panic!("Merge was expected to fail (dimension mismatch), but succeeded"); },
            Err(AgnesError::DimensionMismatch(_)) => { /* expected */ },
            Err(e) => { panic!("Incorrect error: {:?}", e); },
        };
    }

    #[test]
    fn merge_field_collision() {
        let ds1 = sample_emp_table();
        let ds2 = sample_emp_table();

        let (dv1, dv2): (DataView, DataView) = (ds1.into(), ds2.into());
        println!("{}", dv1);
        println!("{}", dv2);
        match dv1.merge(&dv2) {
            Ok(_) => { panic!("Merge expected to fail (field collision), but succeeded"); },
            Err(AgnesError::FieldCollision(fields)) => {
                assert_eq!(fields, vec!["EmpId", "DeptId", "EmpName"]
                    .iter().map(|&s| FieldIdent::Name(s.into())).collect::<Vec<_>>());
            },
            Err(e) => { panic!("Incorrect error: {:?}", e); }
        }
    }

    #[test]
    fn rename() {
        let ds = sample_emp_table();
        let mut dv: DataView = ds.into();
        // println!("{}", dv);
        assert_field_lists_match(dv.fieldnames(), vec!["EmpId", "DeptId", "EmpName"]);
        dv.rename("DeptId", "Department Id").expect("rename failed");
        // println!("{}", dv);
        assert_field_lists_match(dv.fieldnames(), vec!["EmpId", "Department Id", "EmpName"]);
        dv.rename("Department Id", "DeptId").expect("rename failed");
        // println!("{}", dv);
        assert_field_lists_match(dv.fieldnames(), vec!["EmpId", "DeptId", "EmpName"]);
    }

    #[test]
    fn rename_field_collision() {
        let ds = sample_emp_table();
        let mut dv: DataView = ds.into();
        // println!("{}", dv);
        assert_field_lists_match(dv.fieldnames(), vec!["EmpId", "DeptId", "EmpName"]);
        match dv.rename("DeptId", "EmpId") {
            Ok(_) => { panic!("Rename expected to fail (field collision), but succeeded"); },
            Err(AgnesError::FieldCollision(fields)) => {
                assert_eq!(fields, vec!["EmpId"]
                    .iter().map(|&s| FieldIdent::Name(s.into())).collect::<Vec<_>>());
            },
            Err(e) => { panic!("Incorrect error: {:?}", e); }
        }
        // println!("{}", dv);
    }

    #[test]
    fn rename_field_not_found() {
        let ds = sample_emp_table();
        let mut dv: DataView = ds.into();
        // println!("{}", dv);
        assert_field_lists_match(dv.fieldnames(), vec!["EmpId", "DeptId", "EmpName"]);
        match dv.rename("Department Id", "DepartmentId") {
            Ok(_) => { panic!("Rename expected to fail (field not found), but succeeded"); },
            Err(AgnesError::FieldNotFound(field)) => {
                assert_eq!(field, FieldIdent::Name("Department Id".to_string()));
            },
            Err(e) => { panic!("Incorrect error: {:?}", e); }
        }
        // println!("{}", dv);
    }

    #[test]
    fn subview() {
        let ds = sample_emp_table();
        let dv: DataView = ds.into();
        assert_eq!(dv.frames[0].store_ref_count(), 1);
        assert_field_lists_match(dv.fieldnames(), vec!["EmpId", "DeptId", "EmpName"]);

        let subdv1 = dv.v("EmpId");
        assert_eq!(dv.frames[0].store_ref_count(), 2);
        assert_eq!(subdv1.nrows(), 7);
        assert_eq!(subdv1.nfields(), 1);
        let subdv1 = dv.subview("EmpId").expect("subview failed");
        assert_eq!(dv.frames[0].store_ref_count(), 3);
        assert_eq!(subdv1.nrows(), 7);
        assert_eq!(subdv1.nfields(), 1);

        let subdv2 = dv.v(vec!["EmpId", "DeptId"]);
        assert_eq!(dv.frames[0].store_ref_count(), 4);
        assert_eq!(subdv2.nrows(), 7);
        assert_eq!(subdv2.nfields(), 2);
        let subdv2 = dv.subview(vec!["EmpId", "DeptId"]).expect("subview failed");
        assert_eq!(dv.frames[0].store_ref_count(), 5);
        assert_eq!(subdv2.nrows(), 7);
        assert_eq!(subdv2.nfields(), 2);

        let subdv3 = dv.v(vec!["EmpId", "DeptId", "EmpName"]);
        assert_eq!(dv.frames[0].store_ref_count(), 6);
        assert_eq!(subdv3.nrows(), 7);
        assert_eq!(subdv3.nfields(), 3);
        let subdv3 = dv.subview(vec!["EmpId", "DeptId", "EmpName"]).expect("subview failed");
        assert_eq!(dv.frames[0].store_ref_count(), 7);
        assert_eq!(subdv3.nrows(), 7);
        assert_eq!(subdv3.nfields(), 3);

        // Subview of a subview
        let subdv4 = subdv2.v("DeptId");
        assert_eq!(dv.frames[0].store_ref_count(), 8);
        assert_eq!(subdv4.nrows(), 7);
        assert_eq!(subdv4.nfields(), 1);
        let subdv4 = subdv2.subview("DeptId").expect("subview failed");
        assert_eq!(dv.frames[0].store_ref_count(), 9);
        assert_eq!(subdv4.nrows(), 7);
        assert_eq!(subdv4.nfields(), 1);
    }

    #[test]
    fn subview_fail() {
        let ds = sample_emp_table();
        let dv: DataView = ds.into();
        assert_eq!(dv.frames[0].store_ref_count(), 1);
        assert_field_lists_match(dv.fieldnames(), vec!["EmpId", "DeptId", "EmpName"]);

        // "Employee Name" does not exist
        let subdv1 = dv.v(vec!["EmpId", "DeptId", "Employee Name"]);
        assert_eq!(dv.frames[0].store_ref_count(), 2);
        assert_eq!(subdv1.nrows(), 7);
        assert_eq!(subdv1.nfields(), 2);
        match dv.subview(vec!["EmpId", "DeptId", "Employee Name"]) {
            Ok(_) => { panic!("expected error (field not found), but succeeded"); },
            Err(AgnesError::FieldNotFound(field)) => {
                assert_eq!(field, FieldIdent::Name("Employee Name".into()));
            },
            Err(e) => { panic!("Incorrect error: {:?}", e); }
        }

        let subdv2 = dv.v("Nonexistant");
        assert_eq!(dv.frames[0].store_ref_count(), 3);
        assert_eq!(subdv2.nrows(), 7); // still 7 rows, just no fields
        assert_eq!(subdv2.nfields(), 0);
        match dv.subview(vec!["Nonexistant"]) {
            Ok(_) => { panic!("expected error (field not found), but succeeded"); },
            Err(AgnesError::FieldNotFound(field)) => {
                assert_eq!(field, FieldIdent::Name("Nonexistant".into()));
            },
            Err(e) => { panic!("Incorrect error: {:?}", e); }
        }
    }

    #[test]
    fn filter() {
        let ds = sample_emp_table();
        let orig_dv: DataView = ds.into();
        assert_eq!(orig_dv.nrows(), 7);

        // set filtering by department ID
        let mut dv1 = orig_dv.clone();
        dv1.filter("DeptId", |val: &u64| *val == 1).unwrap();
        println!("{}", dv1);
        assert_eq!(dv1.nrows(), 3);
        text::assert_dv_sorted_eq(&dv1, &"EmpName".into(), vec!["Sally", "Bob", "Cara"]);

        // filter a second time
        dv1.filter("EmpId", |val: &u64| *val >= 6).unwrap();
        assert_eq!(dv1.nrows(), 1);
        text::assert_dv_sorted_eq(&dv1, &"EmpName".into(), vec!["Cara"]);

        // that same filter on the original DV has different results
        let mut dv2 = orig_dv.clone();
        dv2.filter("EmpId", |val: &u64| *val >= 6).unwrap();
        assert_eq!(dv2.nrows(), 4);
        text::assert_dv_sorted_eq(&dv2, &"EmpName".into(), vec!["Cara", "Louis", "Louise", "Ann"]);

        // let's try filtering by a different department on dv2
        dv2.filter("DeptId", |val: &u64| *val == 4).unwrap();
        assert_eq!(dv2.nrows(), 2);
        text::assert_dv_sorted_eq(&dv2, &"EmpName".into(), vec!["Louise", "Ann"]);
    }

    #[test]
    fn sort() {
        let orig_dv = sample_merged_emp_table();
        assert_eq!(orig_dv.nrows(), 7);

        // sort by name
        let mut dv1 = orig_dv.clone();
        dv1.sort_by(&"EmpName".into()).unwrap();
        text::assert_dv_eq_vec(&dv1, &"EmpName".into(),
            vec!["Ann", "Bob", "Cara", "Jamie", "Louis", "Louise", "Sally"]
        );
        unsigned::assert_dv_eq_vec(&dv1, &"EmpId".into(),
            vec![10u64, 5, 6, 2, 8, 9, 0]);

        // re-sort by empid
        let mut dv2 = dv1.clone();
        dv2.sort_by(&"EmpId".into()).unwrap();
        text::assert_dv_eq_vec(&dv2, &"EmpName".into(),
            vec!["Sally", "Jamie", "Bob", "Cara", "Louis", "Louise", "Ann"]
        );
        unsigned::assert_dv_eq_vec(&dv2, &"EmpId".into(),
            vec![0u64, 2, 5, 6, 8, 9, 10]);

        // make sure dv1 is still sorted by EmpName
        text::assert_dv_eq_vec(&dv1, &"EmpName".into(),
            vec!["Ann", "Bob", "Cara", "Jamie", "Louis", "Louise", "Sally"]
        );
        unsigned::assert_dv_eq_vec(&dv1, &"EmpId".into(),
            vec![10u64, 5, 6, 2, 8, 9, 0]);

        // starting with sorted by name, sort by vacation hours
        let mut dv3 = dv1.clone();
        dv3.sort_by(&"VacationHrs".into()).unwrap();
        text::assert_dv_eq_vec(&dv3, &"EmpName".into(),
            vec!["Louis", "Louise", "Cara", "Ann", "Sally", "Jamie", "Bob"]
        );
        unsigned::assert_dv_eq_vec(&dv3, &"EmpId".into(),
            vec![8u64, 9, 6, 10, 0, 2, 5]);
    }

    #[test]
    fn filter_sort() {
        let orig_dv = sample_merged_emp_table();
        assert_eq!(orig_dv.nrows(), 7);

        // start by filtering for employees with remaining vacation hours
        let mut dv1 = orig_dv.clone();
        dv1.filter("VacationHrs", |&val: &f64| val >= 0.0).unwrap();
        assert_eq!(dv1.nrows(), 6);
        // only Louis has negative hours, so rest of employees still remain
        text::assert_dv_eq_vec(&dv1, &"EmpName".into(),
            vec!["Sally", "Jamie", "Bob", "Cara", "Louise", "Ann"]
        );

        // next, sort by employee name
        let mut dv2 = dv1.clone();
        dv2.sort_by(&"EmpName".into()).unwrap();
        text::assert_dv_eq_vec(&dv2, &"EmpName".into(),
            vec!["Ann", "Bob", "Cara", "Jamie", "Louise", "Sally"]
        );

        // filter by people in department 1
        let mut dv3 = dv2.clone();
        dv3.filter("DeptId", |&val: &u64| val == 1).unwrap();
        assert_eq!(dv3.nrows(), 3);
        // should just be the people in department 1, in employee name order
        text::assert_dv_eq_vec(&dv3, &"EmpName".into(), vec!["Bob", "Cara", "Sally"]);

        // check that dv1 still has the original ordering
        text::assert_dv_eq_vec(&dv1, &"EmpName".into(),
            vec!["Sally", "Jamie", "Bob", "Cara", "Louise", "Ann"]
        );

        // ok, now filter dv1 by department 1
        dv1.filter("DeptId", |&val: &u64| val == 1).unwrap();
        assert_eq!(dv1.nrows(), 3);
        // should be the people in department 1, but in original name order
        text::assert_dv_eq_vec(&dv1, &"EmpName".into(), vec!["Sally", "Bob", "Cara"]);

        // make sure dv2 hasn't been affected by any of the other changes
        text::assert_dv_eq_vec(&dv2, &"EmpName".into(),
            vec!["Ann", "Bob", "Cara", "Jamie", "Louise", "Sally"]
        );
    }

    #[test]
    fn tmap_closure() {
        let orig_dv = sample_merged_emp_table();

        //FIXME: using a closure with tmap currently requires providing the type annotation for
        // the value passed into the closure (a DataIndex trait object). I believe this is related
        // to this issue: https://github.com/rust-lang/rust/issues/41078.
        let has_jamie = orig_dv.tmap(
            "EmpName",
            |data: &dyn DataIndex<Types, DType=String>|
                DataIterator::new(data).any(|emp_name| emp_name == "Jamie".to_string())
        ).unwrap();
        assert_eq!(has_jamie, true);

        let has_james = orig_dv.tmap(
            "EmpName",
            |data: &dyn DataIndex<Types, DType=String>|
                DataIterator::new(data).any(|emp_name| emp_name == "James".to_string())
        ).unwrap();
        assert_eq!(has_james, false);
    }

    #[test]
    fn tmap_incorrect_field_type() {
        let orig_dv = sample_merged_emp_table();

        match orig_dv.tmap(
            "EmpName",
            |data: &dyn DataIndex<Types, DType=u64>|
                DataIterator::new(data).any(|emp_id| emp_id == 1)
        ) {
            Err(AgnesError::IncompatibleTypes { .. }) => {},
            Err(_) => { panic!["wrong error when calling tmap() with incorrect type"]; },
            Ok(_) => { panic!["expected error when calling tmap() with incorrect type, but \
                               received result"]; }
        }
    }

    #[test]
    fn test_fields_macro() {
        // let orig_dv = sample_merged_emp_table();
        let field_list = hashable_fields!["EmpName" => String, "EmpId" => u64];
        println!("{:?}", field_list);
    }
}
