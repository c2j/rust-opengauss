use rust_decimal_1::Decimal;
use std::str::FromStr;

use crate::types::test_type;

#[tokio::test]
async fn test_rust_decimal_params() {
    test_type(
        "NUMERIC",
        &[
            (
                Some(Decimal::from_str("3950.123456").unwrap()),
                "3950.123456",
            ),
            (Some(Decimal::from_str("3950").unwrap()), "3950"),
            (Some(Decimal::from_str("0.1").unwrap()), "0.1"),
            (Some(Decimal::from_str("0.0001").unwrap()), "0.0001"),
            (Some(Decimal::from_str("-100").unwrap()), "-100"),
            (Some(Decimal::from_str("-123.456").unwrap()), "-123.456"),
            (Some(Decimal::from_str("119996.25").unwrap()), "119996.25"),
            (Some(Decimal::from_str("1000000").unwrap()), "1000000"),
            (
                Some(Decimal::from_str("9999999.99999").unwrap()),
                "9999999.99999",
            ),
            (
                Some(Decimal::from_str("18446744073709551615").unwrap()),
                "18446744073709551615",
            ),
            (
                Some(Decimal::from_str("-18446744073709551615").unwrap()),
                "-18446744073709551615",
            ),
            (None, "NULL"),
        ],
    )
    .await
}
