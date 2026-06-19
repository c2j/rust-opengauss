use bytes::BytesMut;
use std::error::Error;

use crate::{FromSql, IsNull, ToSql, Type};

// Sign constants from PostgreSQL numeric.c
const NUMERIC_NEG: u16 = 0x4000;
const NUMERIC_SPECIAL: u16 = 0xC000;
const NUMERIC_NAN: u16 = 0xC000;
const NUMERIC_PINF: u16 = 0xD000;
const NUMERIC_NINF: u16 = 0xF000;

impl<'a> FromSql<'a> for rust_decimal_1::Decimal {
    fn from_sql(_: &Type, raw: &[u8]) -> Result<Self, Box<dyn Error + Sync + Send>> {
        if raw.len() < 8 {
            return Err("numeric value requires at least 8 bytes".into());
        }

        let num_groups = u16::from_be_bytes([raw[0], raw[1]]) as usize;
        let weight = i16::from_be_bytes([raw[2], raw[3]]);
        let sign = u16::from_be_bytes([raw[4], raw[5]]);
        let dscale = u16::from_be_bytes([raw[6], raw[7]]);

        // Check for special values (NaN, +Inf, -Inf)
        if sign & NUMERIC_SPECIAL == NUMERIC_SPECIAL {
            return match sign {
                NUMERIC_NAN => Err("NaN is not supported for rust_decimal::Decimal".into()),
                NUMERIC_PINF => Err("+Infinity is not supported for rust_decimal::Decimal".into()),
                NUMERIC_NINF => Err("-Infinity is not supported for rust_decimal::Decimal".into()),
                _ => Err("unknown special numeric value".into()),
            };
        }

        // Validate length
        let expected_len = 8 + num_groups * 2;
        if raw.len() < expected_len {
            return Err("numeric value truncated".into());
        }

        // Read digit groups (0..=9999 each)
        let mut digits = Vec::with_capacity(num_groups);
        for i in 0..num_groups {
            let offset = 8 + i * 2;
            let digit = u16::from_be_bytes([raw[offset], raw[offset + 1]]);
            digits.push(digit);
        }

        // Build the whole integer from base-10000 digits using u128 arithmetic.
        // whole = d_0 * 10000^(n-1) + d_1 * 10000^(n-2) + ... + d_{n-1}
        let mut whole: u128 = 0;
        for &digit in &digits {
            whole = whole * 10000 + digit as u128;
        }

        if num_groups == 0 || whole == 0 {
            let mut value = rust_decimal_1::Decimal::ZERO;
            if sign == NUMERIC_NEG {
                value.set_sign_negative(true);
            }
            value.rescale(u32::from(dscale));
            return Ok(value);
        }

        // The decimal value = whole * 10000^(weight - num_groups + 1)
        //                   = whole * 10^(4 * (weight - num_groups + 1))
        let adjust = weight as i32 - num_groups as i32 + 1;

        let mantissa: i128;
        let scale: u32;

        if adjust > 0 {
            let exp = u32::try_from(4 * adjust).map_err(|_| "numeric value exponent overflow")?;
            let factor = 10u128
                .checked_pow(exp)
                .ok_or("numeric value too large for rust_decimal::Decimal")?;
            let scaled = whole
                .checked_mul(factor)
                .ok_or("numeric value too large for rust_decimal::Decimal")?;
            if scaled > i128::MAX as u128 {
                return Err("numeric value too large for rust_decimal::Decimal".into());
            }
            mantissa = scaled as i128;
            scale = 0;
        } else if adjust < 0 {
            if whole > i128::MAX as u128 {
                return Err("numeric value too large for rust_decimal::Decimal".into());
            }
            mantissa = whole as i128;
            let s = u32::try_from(4 * (-adjust)).map_err(|_| "numeric value scale overflow")?;
            scale = s;
        } else {
            if whole > i128::MAX as u128 {
                return Err("numeric value too large for rust_decimal::Decimal".into());
            }
            mantissa = whole as i128;
            scale = 0;
        }

        let mut value = rust_decimal_1::Decimal::try_from_i128_with_scale(mantissa, scale)?;

        if sign == NUMERIC_NEG {
            value.set_sign_negative(true);
        }

        value.rescale(u32::from(dscale));

        Ok(value)
    }

    accepts!(NUMERIC);
}

impl ToSql for rust_decimal_1::Decimal {
    fn to_sql(&self, _: &Type, w: &mut BytesMut) -> Result<IsNull, Box<dyn Error + Sync + Send>> {
        let scale = self.scale();

        if self.is_zero() {
            // Zero: num_groups=0, weight=0, sign=0, dscale=0
            w.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
            return Ok(IsNull::No);
        }

        let negative = self.is_sign_negative();
        let mantissa = self.mantissa().unsigned_abs();

        // Align scale to multiple of 4 (base-10000 boundary)
        let align = scale % 4;
        let aligned_mantissa = if align == 0 {
            mantissa
        } else {
            let factor = 10u128.pow(4 - align);
            mantissa
                .checked_mul(factor)
                .ok_or("numeric value overflow during encoding")?
        };

        // Convert aligned mantissa to base-10000 digits (LSB first)
        let mut digits_lsb: Vec<u16> = Vec::new();
        let mut m = aligned_mantissa;
        while m > 0 {
            digits_lsb.push((m % 10000) as u16);
            m /= 10000;
        }

        // Reverse to MSB-first
        digits_lsb.reverse();

        // Compute weight = num_groups - ceil(scale/4) - 1
        let num_groups_initial = digits_lsb.len();
        let scale_ceil4 = (scale as usize).div_ceil(4);
        let weight = num_groups_initial as i16 - scale_ceil4 as i16 - 1;

        // Trim trailing zero digit groups after the decimal point.
        // A group is "after the decimal point" if its index > weight.
        // The number of integer groups = max(weight + 1, 0).
        let integer_groups = if weight >= -1 {
            (weight + 1) as usize
        } else {
            0
        };
        while digits_lsb.len() > integer_groups && digits_lsb.last() == Some(&0) {
            digits_lsb.pop();
        }

        let num_groups = digits_lsb.len() as u16;

        // Write header (big-endian)
        w.extend_from_slice(&num_groups.to_be_bytes()); // num_groups (2 bytes)
        w.extend_from_slice(&weight.to_be_bytes()); // weight (2 bytes)
        let sign = if negative { NUMERIC_NEG } else { 0x0000 };
        w.extend_from_slice(&sign.to_be_bytes()); // sign (2 bytes)
        w.extend_from_slice(&(scale as u16).to_be_bytes()); // dscale (2 bytes)

        // Write digits (each as i16 big-endian)
        for &digit in &digits_lsb {
            w.extend_from_slice(&(digit as i16).to_be_bytes());
        }

        Ok(IsNull::No)
    }

    accepts!(NUMERIC);
    to_sql_checked!();
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal_1::Decimal;
    use std::str::FromStr;

    /// Mirrors the ToSql logic to generate expected bytes for a Decimal value.
    fn dec_to_bytes(d: &Decimal) -> Vec<u8> {
        let scale = d.scale();

        if d.is_zero() {
            return vec![0, 0, 0, 0, 0, 0, 0, 0];
        }

        let negative = d.is_sign_negative();
        let mantissa = d.mantissa().unsigned_abs();

        let align = scale % 4;
        let aligned_mantissa = if align == 0 {
            mantissa
        } else {
            let factor = 10u128.pow(4 - align);
            mantissa.checked_mul(factor).unwrap()
        };

        let mut digits_lsb: Vec<u16> = Vec::new();
        let mut m = aligned_mantissa;
        while m > 0 {
            digits_lsb.push((m % 10000) as u16);
            m /= 10000;
        }
        digits_lsb.reverse();

        let num_groups_initial = digits_lsb.len();
        let scale_ceil4 = (scale as usize).div_ceil(4);
        let weight = num_groups_initial as i16 - scale_ceil4 as i16 - 1;

        let integer_groups = if weight >= -1 {
            (weight + 1) as usize
        } else {
            0
        };
        while digits_lsb.len() > integer_groups && digits_lsb.last() == Some(&0) {
            digits_lsb.pop();
        }

        let mut buf = Vec::with_capacity(8 + digits_lsb.len() * 2);
        buf.extend_from_slice(&(digits_lsb.len() as u16).to_be_bytes());
        buf.extend_from_slice(&weight.to_be_bytes());
        let sign = if negative { NUMERIC_NEG } else { 0x0000 };
        buf.extend_from_slice(&sign.to_be_bytes());
        buf.extend_from_slice(&(scale as u16).to_be_bytes());
        for &digit in &digits_lsb {
            buf.extend_from_slice(&(digit as i16).to_be_bytes());
        }
        buf
    }

    #[test]
    fn from_sql_3950_123456() {
        let bytes = vec![
            0x00, 0x03, 0x00, 0x00, 0x00, 0x00, 0x00, 0x06, 0x0F, 0x6E, 0x04, 0xD2, 0x15, 0xE0,
        ];
        let d = Decimal::from_sql(&Type::NUMERIC, &bytes).unwrap();
        assert_eq!(d.to_string(), "3950.123456");
    }

    #[test]
    fn from_sql_integer() {
        let d = Decimal::from_sql(&Type::NUMERIC, &dec_to_bytes(&Decimal::from(3950i64))).unwrap();
        assert_eq!(d, Decimal::from(3950i64));
    }

    #[test]
    fn from_sql_small_fraction_0_1() {
        let d = Decimal::from_sql(
            &Type::NUMERIC,
            &dec_to_bytes(&Decimal::from_str("0.1").unwrap()),
        )
        .unwrap();
        assert_eq!(d, Decimal::from_str("0.1").unwrap());
    }

    #[test]
    fn from_sql_small_fraction_0_0001() {
        let d = Decimal::from_sql(
            &Type::NUMERIC,
            &dec_to_bytes(&Decimal::from_str("0.0001").unwrap()),
        )
        .unwrap();
        assert_eq!(d, Decimal::from_str("0.0001").unwrap());
    }

    #[test]
    fn from_sql_negative_100() {
        let d = Decimal::from_sql(&Type::NUMERIC, &dec_to_bytes(&Decimal::from(-100i64))).unwrap();
        assert_eq!(d, Decimal::from(-100i64));
    }

    #[test]
    fn from_sql_negative_123_456() {
        let val = Decimal::from_str("-123.456").unwrap();
        let d = Decimal::from_sql(&Type::NUMERIC, &dec_to_bytes(&val)).unwrap();
        assert_eq!(d, val);
    }

    #[test]
    fn from_sql_zero() {
        let bytes = vec![0, 0, 0, 0, 0, 0, 0, 0];
        let d = Decimal::from_sql(&Type::NUMERIC, &bytes).unwrap();
        assert_eq!(d, Decimal::ZERO);
    }

    #[test]
    fn from_sql_1000000() {
        let val = Decimal::from(1_000_000i64);
        let d = Decimal::from_sql(&Type::NUMERIC, &dec_to_bytes(&val)).unwrap();
        assert_eq!(d, val);
    }

    #[test]
    fn from_sql_9999999_99999() {
        let val = Decimal::from_str("9999999.99999").unwrap();
        let d = Decimal::from_sql(&Type::NUMERIC, &dec_to_bytes(&val)).unwrap();
        assert_eq!(d, val);
    }

    #[test]
    fn from_sql_u64_max() {
        let val = Decimal::from(u64::MAX);
        let d = Decimal::from_sql(&Type::NUMERIC, &dec_to_bytes(&val)).unwrap();
        assert_eq!(d, val);
    }

    #[test]
    fn from_sql_nan() {
        let bytes = vec![0, 0, 0, 0, 0xC0, 0x00, 0, 0];
        let result = Decimal::from_sql(&Type::NUMERIC, &bytes);
        assert!(result.is_err());
        assert!(
            result.err().unwrap().to_string().contains("NaN"),
            "expected NaN error"
        );
    }

    #[test]
    fn from_sql_pos_inf() {
        let bytes = vec![0, 0, 0, 0, 0xD0, 0x00, 0, 0];
        let result = Decimal::from_sql(&Type::NUMERIC, &bytes);
        assert!(result.is_err());
        assert!(
            result.err().unwrap().to_string().contains("Infinity"),
            "expected Infinity error"
        );
    }

    #[test]
    fn from_sql_neg_inf() {
        let bytes = vec![0, 0, 0, 0, 0xF0, 0x00, 0, 0];
        let result = Decimal::from_sql(&Type::NUMERIC, &bytes);
        assert!(result.is_err());
        assert!(
            result.err().unwrap().to_string().contains("Infinity"),
            "expected Infinity error"
        );
    }

    #[test]
    fn to_sql_3950_123456_header() {
        let d = Decimal::from_str("3950.123456").unwrap();

        let mut buf = BytesMut::new();
        let result = ToSql::to_sql(&d, &Type::NUMERIC, &mut buf).unwrap();
        assert!(matches!(result, IsNull::No));

        assert_eq!(buf[0..2], [0x00, 0x03], "num_groups should be 3");
        assert_eq!(buf[2..4], [0x00, 0x00], "weight should be 0");
        assert_eq!(buf[4..6], [0x00, 0x00], "sign should be positive");
        assert_eq!(buf[6..8], [0x00, 0x06], "dscale should be 6");
        assert_eq!(buf[8..10], [0x0F, 0x6E], "first digit should be 3950");
    }

    #[test]
    fn to_sql_zero() {
        let mut buf = BytesMut::new();
        let result = ToSql::to_sql(&Decimal::ZERO, &Type::NUMERIC, &mut buf).unwrap();
        assert!(matches!(result, IsNull::No));
        assert_eq!(buf.as_ref(), &[0, 0, 0, 0, 0, 0, 0, 0]);
    }

    #[test]
    fn to_sql_negative() {
        let d = Decimal::from(-100i64);
        let bytes = dec_to_bytes(&d);
        assert_eq!(bytes[4..6], [0x40, 0x00], "sign should be negative");
    }

    #[test]
    fn roundtrip_3950_123456() {
        let val = Decimal::from_str("3950.123456").unwrap();
        let bytes = dec_to_bytes(&val);
        let back = Decimal::from_sql(&Type::NUMERIC, &bytes).unwrap();
        assert_eq!(val, back);
    }

    #[test]
    fn roundtrip_integer() {
        let val = Decimal::from(3950i64);
        let bytes = dec_to_bytes(&val);
        let back = Decimal::from_sql(&Type::NUMERIC, &bytes).unwrap();
        assert_eq!(val, back);
    }

    #[test]
    fn roundtrip_0_1() {
        let val = Decimal::from_str("0.1").unwrap();
        let bytes = dec_to_bytes(&val);
        let back = Decimal::from_sql(&Type::NUMERIC, &bytes).unwrap();
        assert_eq!(val, back);
    }

    #[test]
    fn roundtrip_0_0001() {
        let val = Decimal::from_str("0.0001").unwrap();
        let bytes = dec_to_bytes(&val);
        let back = Decimal::from_sql(&Type::NUMERIC, &bytes).unwrap();
        assert_eq!(val, back);
    }

    #[test]
    fn roundtrip_negative_100() {
        let val = Decimal::from(-100i64);
        let bytes = dec_to_bytes(&val);
        let back = Decimal::from_sql(&Type::NUMERIC, &bytes).unwrap();
        assert_eq!(val, back);
    }

    #[test]
    fn roundtrip_negative_123_456() {
        let val = Decimal::from_str("-123.456").unwrap();
        let bytes = dec_to_bytes(&val);
        let back = Decimal::from_sql(&Type::NUMERIC, &bytes).unwrap();
        assert_eq!(val, back);
    }

    #[test]
    fn roundtrip_zero() {
        let bytes = dec_to_bytes(&Decimal::ZERO);
        let back = Decimal::from_sql(&Type::NUMERIC, &bytes).unwrap();
        assert_eq!(back, Decimal::ZERO);
    }

    #[test]
    fn roundtrip_1000000() {
        let val = Decimal::from(1_000_000i64);
        let bytes = dec_to_bytes(&val);
        let back = Decimal::from_sql(&Type::NUMERIC, &bytes).unwrap();
        assert_eq!(val, back);
    }

    #[test]
    fn roundtrip_9999999_99999() {
        let val = Decimal::from_str("9999999.99999").unwrap();
        let bytes = dec_to_bytes(&val);
        let back = Decimal::from_sql(&Type::NUMERIC, &bytes).unwrap();
        assert_eq!(val, back);
    }

    #[test]
    fn roundtrip_u64_max() {
        let val = Decimal::from(u64::MAX);
        let bytes = dec_to_bytes(&val);
        let back = Decimal::from_sql(&Type::NUMERIC, &bytes).unwrap();
        assert_eq!(val, back);
    }

    #[test]
    fn accepts_only_numeric() {
        use crate::FromSql;
        assert!(<Decimal as FromSql>::accepts(&Type::NUMERIC));
        assert!(!<Decimal as FromSql>::accepts(&Type::INT4));
        assert!(!<Decimal as FromSql>::accepts(&Type::TEXT));
        assert!(!<Decimal as FromSql>::accepts(&Type::FLOAT8));
        assert!(!<Decimal as FromSql>::accepts(&Type::VARCHAR));
    }
}
