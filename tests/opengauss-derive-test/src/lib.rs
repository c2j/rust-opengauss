#![cfg(test)]

#[cfg(feature = "integration")]
use opengauss::Client;
#[cfg(feature = "integration")]
use opengauss_types::{FromSqlOwned, ToSql};
#[cfg(feature = "integration")]
use std::fmt;

#[cfg(feature = "integration")]
mod composites;
#[cfg(feature = "integration")]
mod domains;
#[cfg(feature = "integration")]
mod enums;
#[cfg(feature = "integration")]
mod transparent;

#[cfg(feature = "integration")]
pub fn test_type<T, S>(conn: &mut Client, sql_type: &str, checks: &[(T, S)])
where
    T: PartialEq + FromSqlOwned + ToSql + Sync,
    S: fmt::Display,
{
    for (val, repr) in checks.iter() {
        let stmt = conn
            .prepare(&format!("SELECT {}::{}", *repr, sql_type))
            .unwrap();
        let result = conn.query_one(&stmt, &[]).unwrap().get(0);
        assert_eq!(val, &result);

        let stmt = conn.prepare(&format!("SELECT $1::{sql_type}")).unwrap();
        let result = conn.query_one(&stmt, &[val]).unwrap().get(0);
        assert_eq!(val, &result);
    }
}

#[cfg(feature = "integration")]
pub fn test_type_asymmetric<T, F, S, C>(
    conn: &mut Client,
    sql_type: &str,
    checks: &[(T, S)],
    cmp: C,
) where
    T: ToSql + Sync,
    F: FromSqlOwned,
    S: fmt::Display,
    C: Fn(&T, &F) -> bool,
{
    for (val, repr) in checks.iter() {
        let stmt = conn
            .prepare(&format!("SELECT {}::{}", *repr, sql_type))
            .unwrap();
        let result: F = conn.query_one(&stmt, &[]).unwrap().get(0);
        assert!(cmp(val, &result));

        let stmt = conn.prepare(&format!("SELECT $1::{sql_type}")).unwrap();
        let result: F = conn.query_one(&stmt, &[val]).unwrap().get(0);
        assert!(cmp(val, &result));
    }
}

#[test]
fn compile_fail() {
    trybuild::TestCases::new().compile_fail("src/compile-fail/*.rs");
}
