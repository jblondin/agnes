//! CSV-based source and reader objects and implentation.

use std::collections::HashMap;
use std::fmt::Debug;
use std::path::PathBuf;
use std::str::FromStr;

use csv_sniffer::metadata::Metadata;
use csv_sniffer::Sniffer;

use cons::*;
use error::*;
use field::FieldIdent;
use fieldlist::{FieldDesignator, FieldPayloadCons, FieldSchema, SchemaCons};
use frame::SimpleFrameFields;
use label::{TypedValue, Valued};
use source::decode::decode;
use source::file::{FileLocator, LocalFileReader, Uri};
use store::{AssocFrameLookup, AssocStorage, DataStore, IntoView, PushFrontFromValueIter};
use value::Value;

/// CSV Data source. Contains location of data file, and computes CSV metadata. Can be turned into
/// `CsvReader` object.
#[derive(Debug, Clone)]
pub struct CsvSource {
    // File source object for the CSV file
    src: FileLocator,
    // CSV file metadata (from `csv-sniffer` crate)
    metadata: Metadata,
}

impl CsvSource {
    /// Create a new `CsvSource` object with provided file location. This constructor will analyze
    /// (sniff) the file to detect its metadata (delimiter, quote character, preamble, etc.)
    ///
    /// # Error
    /// Fails if unable to open the file at the provided location, or if CSV analysis fails.
    pub fn new<L: Into<FileLocator>>(loc: L) -> Result<CsvSource> {
        let loc = loc.into();
        //TODO: make sample size configurable?
        let mut file_reader = LocalFileReader::new(&loc)?;
        let metadata = Sniffer::new().sniff_reader(&mut file_reader)?;

        Ok(CsvSource { src: loc, metadata })
    }
    /// Return the compute `Metadata` for this CSV source.
    pub fn metadata(&self) -> &Metadata {
        &self.metadata
    }
}

/// Type alias for [Cons](../../cons/struct.Cons.html)-list specifying label, data type, and source
/// index information of a CSV data source.
pub type CsvSrcSchemaCons<Label, DType, Tail> = FieldPayloadCons<Label, DType, usize, Tail>;

/// A trait for converting an object into a [CsvSrcSchemaCons](type.CsvSrcSchemaCons.html).
pub trait IntoCsvSrcSchema {
    /// Resultant `CsvSrcSchemaCons` object.
    type CsvSrcSchema;

    /// Convert this into a `CsvSrcSchemaCons` cons-list. `headers` is a map of column header names
    /// to column indices. `num_fields` is the number of columns in the CSV file (for checking for
    /// indexing errors).
    fn into_csv_src_schema(
        self,
        headers: &HashMap<String, usize>,
        num_fields: usize,
    ) -> Result<Self::CsvSrcSchema>;
}
impl IntoCsvSrcSchema for Nil {
    type CsvSrcSchema = Nil;

    fn into_csv_src_schema(
        self,
        _headers: &HashMap<String, usize>,
        _num_fields: usize,
    ) -> Result<Nil> {
        Ok(Nil)
    }
}

impl<Label, DType, Tail> IntoCsvSrcSchema for SchemaCons<Label, DType, Tail>
where
    Tail: IntoCsvSrcSchema,
{
    type CsvSrcSchema = CsvSrcSchemaCons<Label, DType, Tail::CsvSrcSchema>;

    fn into_csv_src_schema(
        self,
        headers: &HashMap<String, usize>,
        num_fields: usize,
    ) -> Result<CsvSrcSchemaCons<Label, DType, Tail::CsvSrcSchema>> {
        let idx = match *self.head.value_ref() {
            FieldDesignator::Expr(ref s) => *headers
                .get(s)
                .ok_or(AgnesError::FieldNotFound(FieldIdent::Name(s.to_string())))?,
            FieldDesignator::Idx(idx) => {
                if idx >= num_fields {
                    return Err(AgnesError::IndexError {
                        index: idx,
                        len: num_fields,
                    });
                };
                idx
            }
        };
        Ok(Cons {
            head: TypedValue::from(idx).into(),
            tail: self.tail.into_csv_src_schema(headers, num_fields)?,
        })
    }
}

/// A trait for building a [DataStore](../../store/struct.DataStore.html) from a
/// [CsvSrcSchemaCons](type.CsvSrcSchemaCons.html).
pub trait BuildDStore {
    /// `Fields` type parameter of the resultant `DataStore`.
    type OutputFields: AssocStorage;

    /// Builds a `DataStore` from the source schema (`self`) and a CSV source `src`.
    fn build(&mut self, src: &CsvSource) -> Result<DataStore<Self::OutputFields>>;
}
impl BuildDStore for Nil {
    type OutputFields = Nil;
    fn build(&mut self, _src: &CsvSource) -> Result<DataStore<Nil>> {
        Ok(DataStore::<Nil>::empty())
    }
}
impl<Label, DType, Tail> BuildDStore for CsvSrcSchemaCons<Label, DType, Tail>
where
    Tail: BuildDStore,
    DataStore<<Tail as BuildDStore>::OutputFields>: PushFrontFromValueIter<Label, DType>,
    Tail::OutputFields: PushBack<FieldSchema<Label, DType>>,
    <Tail::OutputFields as PushBack<FieldSchema<Label, DType>>>::Output: AssocStorage,
    Label: Debug,
    DType: FromStr + Debug + Default + Clone,
    ParseError: From<<DType as FromStr>::Err>,
{
    type OutputFields = <DataStore<<Tail as BuildDStore>::OutputFields> as PushFrontFromValueIter<
        Label,
        DType,
    >>::OutputFields;

    fn build(&mut self, src: &CsvSource) -> Result<DataStore<Self::OutputFields>> {
        let file_reader = LocalFileReader::new(&src.src)?;
        let mut csv_reader = src.metadata.dialect.open_reader(file_reader)?;
        let ds = self.tail.build(src)?;

        let values: Vec<Value<DType>> = csv_reader
            .byte_records()
            .map(|row| {
                let record = row?;
                let value = decode(record.get(*self.head.value_ref().value_ref()).ok_or_else(
                    || AgnesError::FieldNotFound(FieldIdent::Name(stringify![Field].to_string())),
                )?)?;
                Ok(value)
            })
            .map(|sresult| {
                sresult.and_then(|s| {
                    let trimmed = s.trim();
                    if trimmed.is_empty() {
                        Ok(Value::Na)
                    } else {
                        trimmed
                            .parse::<DType>()
                            .map(|value| Value::Exists(value))
                            .map_err(|e| AgnesError::Parse(e.into()))
                    }
                })
            })
            .collect::<Result<_>>()?;
        let ds = ds.push_front_from_value_iter::<Label, DType, _, _>(values);

        Ok(ds)
    }
}

/// Object for reading CSV sources.
#[derive(Debug)]
pub struct CsvReader<CsvSchema> {
    src: CsvSource,
    csv_src_schema: CsvSchema,
}

impl<CsvSrcSchema> CsvReader<CsvSrcSchema>
where
    CsvSrcSchema: Debug,
{
    /// Create a new CSV reader from a CSV source specification. This will process header row (if
    /// exists), and verify the fields specified in the `CsvSource` object exist in this CSV file.
    pub fn new<Schema>(src: &CsvSource, schema: Schema) -> Result<CsvReader<Schema::CsvSrcSchema>>
    where
        Schema: IntoCsvSrcSchema<CsvSrcSchema = CsvSrcSchema>,
    {
        let file_reader = LocalFileReader::new(&src.src)?;
        let mut csv_reader = src.metadata.dialect.open_reader(file_reader)?;

        debug_assert_eq!(src.metadata.num_fields, src.metadata.types.len());

        let headers = if src.metadata.dialect.header.has_header_row {
            let headers = csv_reader.headers()?;
            if headers.len() != src.metadata.num_fields {
                return Err(AgnesError::CsvDialect(
                    "header row does not match sniffed number of fields in CSV file".into(),
                ));
            }
            headers
                .iter()
                .enumerate()
                .map(|(i, s)| (s.to_string(), i))
                .collect::<HashMap<_, _>>()
        } else {
            HashMap::new()
        };
        let csv_src_schema = schema.into_csv_src_schema(&headers, src.metadata.num_fields)?;

        Ok(CsvReader {
            //TODO: remove source from here
            src: src.clone(),
            csv_src_schema,
        })
    }

    /// Read a `CsvSource` into a `DataStore` object.
    pub fn read(&mut self) -> Result<DataStore<CsvSrcSchema::OutputFields>>
    where
        CsvSrcSchema: BuildDStore,
    {
        self.csv_src_schema.build(&self.src)
    }
}

/// Utility function for loading a CSV file from a [FileLocator](../file/enum.FileLocator.html).
///
/// Fails if unable to find or read file at location specified.
pub fn load_csv<L: Into<FileLocator>, Schema>(
    loc: L,
    schema: Schema,
) -> Result<<DataStore<<Schema::CsvSrcSchema as BuildDStore>::OutputFields> as IntoView>::Output>
where
    Schema: IntoCsvSrcSchema,
    Schema::CsvSrcSchema: BuildDStore + Debug,
    <Schema::CsvSrcSchema as BuildDStore>::OutputFields: AssocFrameLookup + SimpleFrameFields,
{
    let source = CsvSource::new(loc)?;
    let mut csv_reader = CsvReader::new(&source, schema)?;
    Ok(csv_reader.read()?.into_view())
}

/// Utility function for loading a CSV file from a URI string.
///
/// Fails if unable to parse `uri`, or if unable to find or read file at the location specified.
pub fn load_csv_from_uri<Schema>(
    uri: &str,
    schema: Schema,
) -> Result<<DataStore<<Schema::CsvSrcSchema as BuildDStore>::OutputFields> as IntoView>::Output>
where
    Schema: IntoCsvSrcSchema,
    Schema::CsvSrcSchema: BuildDStore + Debug,
    <Schema::CsvSrcSchema as BuildDStore>::OutputFields: AssocFrameLookup + SimpleFrameFields,
{
    load_csv(Uri::from_uri(uri.parse::<hyper::Uri>()?)?, schema)
}

/// Utility function for loading a CSV file from a local file path.
///
/// Fails if unable to find or read file at the location specified.
pub fn load_csv_from_path<P, Schema>(
    path: P,
    schema: Schema,
) -> Result<<DataStore<<Schema::CsvSrcSchema as BuildDStore>::OutputFields> as IntoView>::Output>
where
    P: Into<PathBuf>,
    Schema: IntoCsvSrcSchema,
    Schema::CsvSrcSchema: BuildDStore + Debug,
    <Schema::CsvSrcSchema as BuildDStore>::OutputFields: AssocFrameLookup + SimpleFrameFields,
{
    load_csv(path.into(), schema)
}
