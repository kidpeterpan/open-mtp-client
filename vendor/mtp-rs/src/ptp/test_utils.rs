//! Test utilities and macros for PTP module tests.
//!
//! This module provides macros for common test patterns, reducing boilerplate
//! across the codebase.

/// Generate a proptest that verifies `Type::from_bytes` doesn't panic on arbitrary input.
///
/// This is the most common fuzz test pattern: feed random bytes to a parser and
/// verify it handles them gracefully (returns Ok or Err, but never panics).
///
/// # Usage
///
/// ```ignore
/// fuzz_bytes!(data_container, DataContainer, 100);
/// ```
///
/// Expands to a proptest named `fuzz_data_container` that feeds 0-100 random bytes
/// to `DataContainer::from_bytes`.
#[cfg(test)]
#[macro_export]
macro_rules! fuzz_bytes {
    ($test_name:ident, $type:ty, $max_size:expr) => {
        proptest::proptest! {
            #[test]
            fn $test_name(bytes in proptest::collection::vec(proptest::arbitrary::any::<u8>(), 0..$max_size)) {
                let _ = <$type>::from_bytes(&bytes);
            }
        }
    };
}

/// Generate a proptest that verifies a function doesn't panic on arbitrary byte input.
///
/// Similar to `fuzz_bytes!` but for standalone functions instead of methods.
///
/// # Usage
///
/// ```ignore
/// fuzz_bytes_fn!(fuzz_container_type, container_type, 100);
/// ```
#[cfg(test)]
#[macro_export]
macro_rules! fuzz_bytes_fn {
    ($test_name:ident, $fn_name:ident, $max_size:expr) => {
        proptest::proptest! {
            #[test]
            fn $test_name(bytes in proptest::collection::vec(proptest::arbitrary::any::<u8>(), 0..$max_size)) {
                let _ = $fn_name(&bytes);
            }
        }
    };
}

/// Generate proptest for types with two-argument `from_bytes(bytes, type_hint)`.
///
/// Used for parsers that need a type discriminator, like PropertyValue or PropertyRange.
///
/// # Usage
///
/// ```ignore
/// fuzz_bytes_with_type!(
///     fuzz_property_value,
///     PropertyValue,
///     PropertyDataType,
///     [Int8, Uint8, Int16, Uint16, Int32, Uint32, Int64, Uint64, String],
///     20
/// );
/// ```
#[cfg(test)]
#[macro_export]
macro_rules! fuzz_bytes_with_type {
    ($test_name:ident, $type:ty, $hint_type:ty, [$($variant:ident),+ $(,)?], $max_size:expr) => {
        proptest::proptest! {
            #[test]
            fn $test_name(bytes in proptest::collection::vec(proptest::arbitrary::any::<u8>(), 0..$max_size)) {
                $(
                    let _ = <$type>::from_bytes(&bytes, <$hint_type>::$variant);
                )+
            }
        }
    };
}
