/*!
Useful statistics-calculating traits for fields with numeric data.
*/
use std::ops::{Add, Mul};

use num_traits::{AsPrimitive, Zero};

use access::DataIndex;
use value::Value;

/// A trait for counting NA and existing values in a field.
pub trait NaCount {
    /// Returns the number of NA (missing) values in this field.
    fn num_na(&self) -> usize;
    /// Returns the number of existing (non-missing) values in this field.
    fn num_exists(&self) -> usize;
}

impl<DI> NaCount for DI
where
    DI: DataIndex,
{
    fn num_na(&self) -> usize {
        self.iter().fold(
            0usize,
            |count, value| if value.exists() { count } else { count + 1 },
        )
    }
    fn num_exists(&self) -> usize {
        self.iter().fold(
            0usize,
            |count, value| if value.exists() { count + 1 } else { count },
        )
    }
}

/// A trait for computing the sum of values in a field.
pub trait Sum {
    /// The data type of the sum result.
    type Output;
    /// Returns the sum of values in this field. Treats missing values as `0`.
    fn sum(&self) -> Self::Output;
}

impl<DI> Sum for DI
where
    DI: DataIndex,
    DI::DType: for<'a> Add<&'a DI::DType, Output = DI::DType> + Zero,
{
    type Output = <DI as DataIndex>::DType;

    fn sum(&self) -> Self::Output {
        self.iter().fold(
            <<Self as DataIndex>::DType as Zero>::zero(),
            |sum, value| match value {
                Value::Exists(value) => sum + value,
                Value::Na => sum,
            },
        )
    }
}

/// A trait for calculating the arithmetic mean of a field.
pub trait Mean {
    /// Compute the arithmetic mean of a field. Ignores missing values in the computation. If all
    /// values missing, returns `0.0`.
    fn mean(&self) -> f64;
}

impl<DI> Mean for DI
where
    DI: DataIndex + NaCount + Sum,
    <DI as Sum>::Output: AsPrimitive<f64>,
{
    fn mean(&self) -> f64 {
        let nexists = match self.num_exists() {
            0 => {
                return 0.0;
            }
            val => val as f64,
        };
        self.sum().as_() / nexists
    }
}

/// A trait for calculating the sum of squares of values in this field.
pub trait SumSq {
    /// The data type of the sum result.
    type Output;

    /// Calculate the sum of squares of values in this field. Treats missing values as `0`.
    fn sum_sq(&self) -> Self::Output;
}

impl<DI> SumSq for DI
where
    DI: DataIndex,
    DI::DType: for<'a> Add<&'a DI::DType, Output = DI::DType> + Zero,
    for<'a, 'b> &'a DI::DType: Mul<&'b DI::DType, Output = DI::DType>,
{
    type Output = DI::DType;

    fn sum_sq(&self) -> DI::DType {
        self.iter().fold(
            <<Self as DataIndex>::DType as Zero>::zero(),
            |sum, value| match value {
                Value::Exists(value) => sum + value.clone() * value,
                Value::Na => sum,
            },
        )
    }
}

/// A trait for computing the variance and standard deviation of values in a field.
pub trait Variance {
    /// Computes sample variance of this field. Ignores missing values in this computation. If all
    /// values are missing, returns `0.0`.
    fn var(&self) -> f64;
    /// Computes population variance of this field. Ignores missing values in this computation. If
    /// all values are missing, returns `0.0`.
    fn varp(&self) -> f64;
    /// Computes sample standard deviation of this field. Ignores missing values in this
    /// computation. If all values are missing, returns `0.0`.
    fn stdev(&self) -> f64 {
        self.var().sqrt()
    }
    /// Computes population standard deviation of this field. Ignores missing values in this
    /// computation. If all values are missing, returns `0.0`.
    fn stdevp(&self) -> f64 {
        self.varp().sqrt()
    }
}

impl<DI> Variance for DI
where
    DI: DataIndex + SumSq + NaCount + Mean,
    <DI as SumSq>::Output: AsPrimitive<f64>,
{
    fn var(&self) -> f64 {
        let nexists = match self.num_exists() {
            0 => {
                return 0.0;
            }
            val => val as f64,
        };
        let sum_sq = self.sum_sq();
        let mean: f64 = self.mean().as_();
        sum_sq.as_() / (nexists - 1.0) - nexists / (nexists - 1.0) * mean * mean
    }
    fn varp(&self) -> f64 {
        let nexists = match self.num_exists() {
            0 => {
                return 0.0;
            }
            val => val as f64,
        };
        let sum_sq = self.sum_sq();
        let mean: f64 = self.mean().as_();
        sum_sq.as_() / nexists - mean * mean
    }
}

/// A trait for computing the upper and lower extrema values for a field.
pub trait Extrema {
    /// The data type of the upper and lower values.
    type Output;

    /// The minimum value in this field. Returns `None` if no values exist in this field.
    fn min(&self) -> Option<&Self::Output>;
    /// The maximum value in this field. Returns `None` if no values exist in this field.
    fn max(&self) -> Option<&Self::Output>;
}

impl<DI> Extrema for DI
where
    DI: DataIndex,
    DI::DType: PartialOrd,
{
    type Output = DI::DType;

    fn min(&self) -> Option<&DI::DType> {
        if self.num_exists() == 0 {
            return None;
        }
        let mut ret = None;
        for val in self.iter() {
            match (ret, val) {
                (None, Value::Exists(val)) => {
                    ret = Some(val);
                }
                (Some(cur_min), Value::Exists(val)) => {
                    if val < cur_min {
                        ret = Some(val);
                    }
                }
                _ => {}
            }
        }
        ret
    }
    fn max(&self) -> Option<&DI::DType> {
        if self.num_exists() == 0 {
            return None;
        }
        let mut ret = None;
        for val in self.iter() {
            match (ret, val) {
                (None, Value::Exists(val)) => {
                    ret = Some(val);
                }
                (Some(cur_max), Value::Exists(val)) => {
                    if val > cur_max {
                        ret = Some(val);
                    }
                }
                _ => {}
            }
        }
        ret
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use cons::Nil;
    use field::FieldData;
    use select::FieldSelect;
    use store::DataStore;
    use value::Value;

    tablespace![
        pub table foo {
            Foo: f64
        }
    ];

    #[test]
    fn na_count() {
        let dv = DataStore::<Nil>::empty()
            .push_back_from_value_iter::<foo::Foo, _, _, _>(vec![
                Value::Exists(0.0),
                Value::Exists(-5.0),
                Value::Na,
                Value::Na,
                Value::Exists(-3.0),
            ])
            .into_view();
        assert_eq!(dv.field::<foo::Foo>().num_na(), 2);
        assert_eq!(dv.field::<foo::Foo>().num_exists(), 3);
    }

    #[test]
    fn sum() {
        let dv = DataStore::<Nil>::empty()
            .push_back_from_value_iter::<foo::Foo, _, _, _>(vec![
                Value::Exists(0u64),
                Value::Exists(5),
                Value::Na,
                Value::Na,
                Value::Exists(3),
            ])
            .into_view();
        assert_eq!(dv.field::<foo::Foo>().sum(), 8);

        let dv = DataStore::<Nil>::empty()
            .push_back_from_value_iter::<foo::Foo, _, _, _>(vec![
                Value::Exists(0),
                Value::Exists(-5),
                Value::Na,
                Value::Na,
                Value::Exists(-3),
            ])
            .into_view();
        assert_eq!(dv.field::<foo::Foo>().sum(), -8);

        let dv = DataStore::<Nil>::empty()
            .push_back_from_value_iter::<foo::Foo, _, _, _>(vec![
                Value::Exists(true),
                Value::Exists(true),
                Value::Exists(false),
                Value::Na,
                Value::Exists(true),
            ])
            .into_view();
        assert_eq!(
            dv.field::<foo::Foo>()
                .iter()
                .map(|b| if b.exists() && *b.unwrap() { 1 } else { 0 })
                .collect::<FieldData<_>>()
                .sum(),
            3
        );

        let dv = DataStore::<Nil>::empty()
            .push_back_from_value_iter::<foo::Foo, _, _, _>(vec![
                Value::Exists(0.0),
                Value::Exists(-5.0),
                Value::Na,
                Value::Na,
                Value::Exists(-3.0),
            ])
            .into_view();
        assert_eq!(dv.field::<foo::Foo>().sum(), -8.0);
    }

    #[test]
    fn stdev() {
        let dv = DataStore::<Nil>::empty()
            .push_back_from_value_iter::<foo::Foo, _, _, _>(vec![
                Value::Exists(-5.0),
                Value::Exists(-4.0),
                Value::Na,
                Value::Exists(12.0),
                Value::Exists(3.0),
                Value::Na,
                Value::Exists(6.0),
                Value::Exists(0.0),
                Value::Exists(-3.1),
            ])
            .into_view();
        assert!((dv.field::<foo::Foo>().var() - 38.049048).abs() < 1e-6);
        assert!((dv.field::<foo::Foo>().stdev() - 6.168391).abs() < 1e-6);
        assert!((dv.field::<foo::Foo>().varp() - 32.613469).abs() < 1e-6);
        assert!((dv.field::<foo::Foo>().stdevp() - 5.710820).abs() < 1e-6);
        assert!((dv.field::<foo::Foo>().mean() - 1.271429).abs() < 1e-6);
        assert_eq!(dv.field::<foo::Foo>().sum(), 8.9);
    }

    #[test]
    fn min() {
        let dv = DataStore::<Nil>::empty()
            .push_back_from_value_iter::<foo::Foo, _, _, _>(vec![
                Value::Exists(0u32),
                Value::Exists(9),
                Value::Na,
                Value::Na,
                Value::Exists(3),
            ])
            .into_view();
        assert_eq!(dv.field::<foo::Foo>().min(), Some(&0));

        let dv = DataStore::<Nil>::empty()
            .push_back_from_value_iter::<foo::Foo, _, _, _>(vec![
                Value::Exists(0i32),
                Value::Exists(-9),
                Value::Na,
                Value::Na,
                Value::Exists(-3),
            ])
            .into_view();
        assert_eq!(dv.field::<foo::Foo>().min(), Some(&-9));

        let dv = DataStore::<Nil>::empty()
            .push_back_from_value_iter::<foo::Foo, _, _, _>(vec![
                Value::Exists(true),
                Value::Exists(true),
                Value::Exists(false),
                Value::Na,
                Value::Exists(true),
            ])
            .into_view();
        assert_eq!(dv.field::<foo::Foo>().min(), Some(&false));

        let dv = DataStore::<Nil>::empty()
            .push_back_from_value_iter::<foo::Foo, _, _, _>(vec![
                Value::Exists(0.0),
                Value::Exists(-9.0),
                Value::Na,
                Value::Na,
                Value::Exists(-3.0),
            ])
            .into_view();
        assert_eq!(dv.field::<foo::Foo>().min(), Some(&-9.0));

        let dv = DataStore::<Nil>::empty()
            .push_back_from_value_iter::<foo::Foo, f64, _, _>(vec![
                Value::Na,
                Value::Na,
                Value::Na,
                Value::Na,
                Value::Na,
            ])
            .into_view();
        assert_eq!(dv.field::<foo::Foo>().min(), None);
    }

    #[test]
    fn max() {
        let dv = DataStore::<Nil>::empty()
            .push_back_from_value_iter::<foo::Foo, _, _, _>(vec![
                Value::Exists(0u32),
                Value::Exists(9),
                Value::Na,
                Value::Na,
                Value::Exists(3),
            ])
            .into_view();
        assert_eq!(dv.field::<foo::Foo>().max(), Some(&9));

        let dv = DataStore::<Nil>::empty()
            .push_back_from_value_iter::<foo::Foo, _, _, _>(vec![
                Value::Exists(0i64),
                Value::Exists(-9),
                Value::Na,
                Value::Na,
                Value::Exists(-3),
            ])
            .into_view();
        assert_eq!(dv.field::<foo::Foo>().max(), Some(&0));

        let dv = DataStore::<Nil>::empty()
            .push_back_from_value_iter::<foo::Foo, _, _, _>(vec![
                Value::Exists(true),
                Value::Exists(true),
                Value::Exists(false),
                Value::Na,
                Value::Exists(true),
            ])
            .into_view();
        assert_eq!(dv.field::<foo::Foo>().max(), Some(&true));

        let dv = DataStore::<Nil>::empty()
            .push_back_from_value_iter::<foo::Foo, _, _, _>(vec![
                Value::Exists(0.0),
                Value::Exists(-9.0),
                Value::Na,
                Value::Na,
                Value::Exists(-3.0),
            ])
            .into_view();
        assert_eq!(dv.field::<foo::Foo>().max(), Some(&0.0));

        let dv = DataStore::<Nil>::empty()
            .push_back_from_value_iter::<foo::Foo, f64, _, _>(vec![
                Value::Na,
                Value::Na,
                Value::Na,
                Value::Na,
                Value::Na,
            ])
            .into_view();
        assert_eq!(dv.field::<foo::Foo>().max(), None);
    }
}
