//! The PostgreSQL backend (PostgresBackend in mcp_server.py):
//! each run uses its own connection and a READ ONLY transaction
//! that is always rolled back. Pageable statements page through
//! a portal: skip `offset` rows, then fetch `limit + 1`. No SQL
//! text is constructed around the client's statement.

use std::fmt;
use std::str::FromStr;

use serde_json::Value;
use tokio_postgres::types::{FromSql, Kind, Type};
use tokio_postgres::{Config as PgConfig, NoTls, Row};

#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Text(String),
    Bytes(Vec<u8>),
    Json(Value),
    Array(Vec<Cell>),
}

#[derive(Debug, Clone)]
pub struct ToolError(pub String);

impl fmt::Display for ToolError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ToolError {}

pub struct QueryOutcome {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Cell>>,
    pub has_more: bool,
}

pub struct Backend {
    dsn: String,
    timeout_ms: i64,
}

/// A bound parameter for the internal statements (the client's
/// own statements never take parameters).
#[derive(Debug, Clone)]
pub enum Param {
    Text(Option<String>),
    Int(i64),
}

fn db_error_text(error: &tokio_postgres::Error) -> String {
    if let Some(db) = error.as_db_error() {
        format!("{}: ERROR:  {}", db.code().code(), db.message())
    } else {
        "database is unavailable".to_string()
    }
}

impl Backend {
    pub fn new(dsn: String, statement_timeout_ms: i64) -> Self {
        Backend {
            dsn,
            timeout_ms: statement_timeout_ms,
        }
    }

    /// Return (columns, rows, has_more) for up to `limit` rows
    /// starting at `offset`.
    pub fn run(
        &self,
        sql: &str,
        params: &[Param],
        limit: i64,
        offset: i64,
        server_side: bool,
    ) -> Result<QueryOutcome, ToolError> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|_| ToolError("database is unavailable".to_string()))?;
        runtime.block_on(self.run_async(sql, params, limit, offset, server_side))
    }

    async fn run_async(
        &self,
        sql: &str,
        params: &[Param],
        limit: i64,
        offset: i64,
        server_side: bool,
    ) -> Result<QueryOutcome, ToolError> {
        let mut config = PgConfig::from_str(&self.dsn)
            .map_err(|_| ToolError("database is unavailable".to_string()))?;
        config.connect_timeout(std::time::Duration::from_secs(5));
        config.application_name("homelab-mcp-postgres");
        config.options(format!(
            "-c statement_timeout={} -c lock_timeout={} \
             -c idle_in_transaction_session_timeout={} \
             -c default_transaction_read_only=on",
            self.timeout_ms,
            self.timeout_ms.min(2000),
            self.timeout_ms + 5000
        ));
        let (mut client, connection) = config
            .connect(NoTls)
            .await
            .map_err(|_| ToolError("database is unavailable".to_string()))?;
        let driver = tokio::spawn(async move {
            let _ = connection.await;
        });
        let outcome = self
            .run_in_transaction(&mut client, sql, params, limit, offset, server_side)
            .await;
        drop(client);
        driver.abort();
        outcome
    }

    async fn run_in_transaction(
        &self,
        client: &mut tokio_postgres::Client,
        sql: &str,
        params: &[Param],
        limit: i64,
        offset: i64,
        server_side: bool,
    ) -> Result<QueryOutcome, ToolError> {
        let tx = client
            .transaction()
            .await
            .map_err(|error| ToolError(db_error_text(&error)))?;
        let result = async {
            let statement = tx
                .prepare(sql)
                .await
                .map_err(|error| ToolError(db_error_text(&error)))?;
            let columns: Vec<String> = statement
                .columns()
                .iter()
                .map(|column| column.name().to_string())
                .collect();
            if columns.is_empty() {
                return Ok(QueryOutcome {
                    columns,
                    rows: Vec::new(),
                    has_more: false,
                });
            }
            if server_side {
                let portal = tx
                    .bind(&statement, &[])
                    .await
                    .map_err(|error| ToolError(db_error_text(&error)))?;
                let mut skipped = 0i64;
                while skipped < offset {
                    let chunk = ((offset - skipped).min(50_000)) as i32;
                    let batch = tx
                        .query_portal(&portal, chunk)
                        .await
                        .map_err(|error| ToolError(db_error_text(&error)))?;
                    if batch.is_empty() {
                        break;
                    }
                    skipped += batch.len() as i64;
                }
                let fetched = tx
                    .query_portal(&portal, (limit + 1) as i32)
                    .await
                    .map_err(|error| ToolError(db_error_text(&error)))?;
                let has_more = fetched.len() as i64 > limit;
                let rows = fetched
                    .iter()
                    .take(limit as usize)
                    .map(row_to_cells)
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(QueryOutcome {
                    columns,
                    rows,
                    has_more,
                })
            } else {
                if offset != 0 {
                    return Err(ToolError(
                        "this statement does not support pagination".to_string(),
                    ));
                }
                // The two internal statements take parameters;
                // the client's own statements take none.
                let text_params: Vec<Option<String>> = params
                    .iter()
                    .map(|param| match param {
                        Param::Text(value) => value.clone(),
                        Param::Int(_) => None,
                    })
                    .collect();
                let refs: Vec<&(dyn tokio_postgres::types::ToSql + Sync)> = params
                    .iter()
                    .enumerate()
                    .map(
                        |(index, param)| -> &(dyn tokio_postgres::types::ToSql + Sync) {
                            match param {
                                Param::Text(_) => &text_params[index],
                                Param::Int(value) => value,
                            }
                        },
                    )
                    .collect();
                let fetched = tx
                    .query(&statement, refs.as_slice())
                    .await
                    .map_err(|error| ToolError(db_error_text(&error)))?;
                let has_more = fetched.len() as i64 > limit;
                let rows = fetched
                    .iter()
                    .take(limit as usize)
                    .map(row_to_cells)
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(QueryOutcome {
                    columns,
                    rows,
                    has_more,
                })
            }
        }
        .await;
        let _ = tx.rollback().await;
        result
    }
}

fn row_to_cells(row: &Row) -> Result<Vec<Cell>, ToolError> {
    row.columns()
        .iter()
        .enumerate()
        .map(|(index, column)| {
            let ty = column.type_();
            row.try_get::<_, Option<WireValue<'_>>>(index)
                .map_err(|_| ToolError(format!("Cannot read column {}", column.name())))?
                .map_or(Ok(Cell::Null), |v| decode_cell(ty, v.0))
        })
        .collect()
}

// postgres-types restricts Vec<u8> to BYTEA. Read the field once using its
// actual type, including nested arrays and SQL domains. Only SQL NULL is null.
struct WireValue<'a>(&'a [u8]);

impl<'a> FromSql<'a> for WireValue<'a> {
    fn from_sql(
        _ty: &Type,
        bytes: &'a [u8],
    ) -> Result<Self, Box<dyn std::error::Error + Sync + Send>> {
        Ok(Self(bytes))
    }
    fn accepts(_: &Type) -> bool {
        true
    }
}

fn decode_cell(ty: &Type, bytes: &[u8]) -> Result<Cell, ToolError> {
    let invalid = || {
        ToolError(format!(
            "Cannot decode PostgreSQL type {}. Cast this column to text.",
            ty.name()
        ))
    };
    macro_rules! read {
        ($kind:ty) => {
            <$kind as FromSql>::from_sql(ty, bytes).map_err(|_| invalid())?
        };
    }
    Ok(match *ty {
        Type::BOOL => Cell::Bool(read!(bool)),
        Type::INT2 => Cell::Int(i64::from(read!(i16))),
        Type::INT4 => Cell::Int(i64::from(read!(i32))),
        Type::INT8 => Cell::Int(read!(i64)),
        Type::OID => Cell::Int(i64::from(read!(u32))),
        Type::FLOAT4 => Cell::Float(f64::from(read!(f32))),
        Type::FLOAT8 => Cell::Float(read!(f64)),
        Type::NUMERIC if bytes.len() >= 8 => Cell::Text(decode_numeric(bytes)),
        Type::UUID => Cell::Text(read!(uuid::Uuid).to_string()),
        Type::DATE => Cell::Text(read!(chrono::NaiveDate).to_string()),
        Type::TIME => Cell::Text(iso_time(read!(chrono::NaiveTime))),
        Type::TIMESTAMP => {
            let v = read!(chrono::NaiveDateTime);
            Cell::Text(format!("{}T{}", v.date(), iso_time(v.time())))
        }
        Type::TIMESTAMPTZ => {
            let v = read!(chrono::DateTime<chrono::Utc>);
            Cell::Text(format!("{}T{}+00:00", v.date_naive(), iso_time(v.time())))
        }
        Type::INTERVAL if bytes.len() == 16 => Cell::Text(format_interval(
            i32::from_be_bytes(bytes[12..16].try_into().unwrap()),
            i32::from_be_bytes(bytes[8..12].try_into().unwrap()),
            i64::from_be_bytes(bytes[..8].try_into().unwrap()),
        )),
        Type::TIMETZ if bytes.len() == 12 => Cell::Text(format_timetz(
            i64::from_be_bytes(bytes[..8].try_into().unwrap()),
            i32::from_be_bytes(bytes[8..].try_into().unwrap()),
        )),
        Type::BYTEA => Cell::Bytes(bytes.to_vec()),
        Type::JSON | Type::JSONB => Cell::Json(read!(Value)),
        Type::CHAR | Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::UNKNOWN => {
            Cell::Text(std::str::from_utf8(bytes).map_err(|_| invalid())?.into())
        }
        _ => match ty.kind() {
            Kind::Enum(_) => Cell::Text(std::str::from_utf8(bytes).map_err(|_| invalid())?.into()),
            Kind::Domain(base) => decode_cell(base, bytes)?,
            Kind::Array(element) => decode_array(element, bytes).ok_or_else(invalid)??,
            _ => return Err(invalid()),
        },
    })
}

fn decode_array(element: &Type, mut bytes: &[u8]) -> Option<Result<Cell, ToolError>> {
    fn integer(bytes: &mut &[u8]) -> Option<i32> {
        let (value, rest) = bytes.split_at_checked(4)?;
        *bytes = rest;
        Some(i32::from_be_bytes(value.try_into().ok()?))
    }
    fn level(
        element: &Type,
        dimensions: &[usize],
        bytes: &mut &[u8],
    ) -> Option<Result<Cell, ToolError>> {
        let Some((&count, rest)) = dimensions.split_first() else {
            let length = integer(bytes)?;
            if length == -1 {
                return Some(Ok(Cell::Null));
            }
            let (raw, tail) = bytes.split_at_checked(usize::try_from(length).ok()?)?;
            *bytes = tail;
            return Some(decode_cell(element, raw));
        };
        let mut items = Vec::new();
        for _ in 0..count {
            match level(element, rest, bytes)? {
                Ok(cell) => items.push(cell),
                Err(e) => return Some(Err(e)),
            }
        }
        Some(Ok(Cell::Array(items)))
    }
    let ndims = usize::try_from(integer(&mut bytes)?).ok()?;
    if ndims > 6 {
        return None;
    }
    let _flags = integer(&mut bytes)?;
    if integer(&mut bytes)? as u32 != element.oid() {
        return None;
    }
    let mut dimensions = Vec::new();
    for _ in 0..ndims {
        let n = usize::try_from(integer(&mut bytes)?).ok()?;
        if n == 0 {
            return None;
        }
        dimensions.push(n);
        let _lower_bound = integer(&mut bytes)?;
    }
    let value = if dimensions.is_empty() {
        Ok(Cell::Array(Vec::new()))
    } else {
        level(element, &dimensions, &mut bytes)?
    };
    bytes.is_empty().then_some(value)
}

/// Python's time.isoformat(): fraction only when sub-second,
/// six digits.
fn iso_time(time: chrono::NaiveTime) -> String {
    use chrono::Timelike;
    let base = format!(
        "{:02}:{:02}:{:02}",
        time.hour(),
        time.minute(),
        time.second()
    );
    let nanos = time.nanosecond();
    if nanos == 0 {
        base
    } else {
        format!("{base}.{:06}", nanos / 1000)
    }
}

fn format_timetz(micros: i64, zone_seconds: i32) -> String {
    let seconds = micros.div_euclid(1_000_000);
    let sub = micros.rem_euclid(1_000_000);
    let time = format!(
        "{:02}:{:02}:{:02}",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60
    );
    let time = if sub == 0 {
        time
    } else {
        format!("{time}.{sub:06}")
    };
    let sign = if zone_seconds > 0 { "-" } else { "+" };
    let zone = zone_seconds.unsigned_abs();
    format!("{time}{sign}{:02}:{:02}", zone / 3600, (zone / 60) % 60)
}

/// str(datetime.timedelta) for months*30 + days + microseconds
/// (psycopg2's interval conversion).
fn format_interval(months: i32, days: i32, micros: i64) -> String {
    let total_us =
        (i128::from(months) * 30 + i128::from(days)) * 86_400_000_000 + i128::from(micros);
    let days_out = total_us.div_euclid(86_400_000_000);
    let rest = total_us.rem_euclid(86_400_000_000);
    let seconds = rest / 1_000_000;
    let sub = rest % 1_000_000;
    let mut text = format!(
        "{}:{:02}:{:02}",
        seconds / 3600,
        (seconds / 60) % 60,
        seconds % 60
    );
    if sub != 0 {
        text = format!("{text}.{sub:06}");
    }
    if days_out != 0 {
        let plural = if days_out == 1 || days_out == -1 {
            "day"
        } else {
            "days"
        };
        text = format!("{days_out} {plural}, {text}");
    }
    text
}

/// PostgreSQL numeric binary -> plain-notation decimal string
/// (what numeric_out prints, which is what psycopg2 parses into
/// the Decimal whose str() Python returns, exponent cases aside).
pub fn decode_numeric(bytes: &[u8]) -> String {
    if bytes.len() < 8 {
        return "0".to_string();
    }
    let ndigits = u16::from_be_bytes(bytes[0..2].try_into().unwrap()) as usize;
    let weight = i16::from_be_bytes(bytes[2..4].try_into().unwrap()) as i64;
    let sign = u16::from_be_bytes(bytes[4..6].try_into().unwrap());
    let dscale = i16::from_be_bytes(bytes[6..8].try_into().unwrap()) as usize;
    if sign == 0xC000 {
        return "NaN".to_string();
    }
    if sign == 0xD000 {
        return "Infinity".to_string();
    }
    if sign == 0xF000 {
        return "-Infinity".to_string();
    }
    let mut groups: Vec<u16> = Vec::new();
    for index in 0..ndigits {
        let offset = 8 + index * 2;
        if offset + 2 <= bytes.len() {
            groups.push(u16::from_be_bytes(
                bytes[offset..offset + 2].try_into().unwrap(),
            ));
        }
    }
    // Integer groups: exponents weight..=0; fraction: -1 downward.
    let mut int_part = String::new();
    let mut frac_part = String::new();
    if groups.is_empty() {
        int_part.push('0');
    } else {
        for (index, group) in groups.iter().enumerate() {
            let exponent = weight - index as i64;
            if exponent >= 0 {
                if index == 0 {
                    int_part.push_str(&group.to_string());
                } else {
                    int_part.push_str(&format!("{group:04}"));
                }
            } else {
                frac_part.push_str(&format!("{group:04}"));
            }
        }
        if weight >= 0 {
            let missing = (weight - (groups.len() as i64 - 1)).max(0);
            for _ in 0..missing {
                int_part.push_str("0000");
            }
        } else {
            // All groups are fractional; prepend the skipped groups.
            let skipped = (-weight - 1).max(0);
            let mut leading = String::new();
            for _ in 0..skipped {
                leading.push_str("0000");
            }
            frac_part = format!("{leading}{frac_part}");
            int_part.push('0');
        }
        if int_part.is_empty() {
            int_part.push('0');
        }
    }
    // Adjust the fraction to exactly dscale digits.
    if frac_part.len() > dscale {
        frac_part.truncate(dscale);
    }
    while frac_part.len() < dscale {
        frac_part.push('0');
    }
    let prefix = if sign == 0x4000 { "-" } else { "" };
    if dscale == 0 {
        format!("{prefix}{int_part}")
    } else {
        format!("{prefix}{int_part}.{frac_part}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn numeric_bytes(groups: &[u16], weight: i16, sign: u16, dscale: i16) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(groups.len() as u16).to_be_bytes());
        bytes.extend_from_slice(&weight.to_be_bytes());
        bytes.extend_from_slice(&sign.to_be_bytes());
        bytes.extend_from_slice(&dscale.to_be_bytes());
        for group in groups {
            bytes.extend_from_slice(&group.to_be_bytes());
        }
        bytes
    }

    #[test]
    fn numeric_rendering() {
        // 1.50: groups [1, 5000], weight 0, dscale 2.
        assert_eq!(decode_numeric(&numeric_bytes(&[1, 5000], 0, 0, 2)), "1.50");
        // 0.001: groups [10], weight -1, dscale 3.
        assert_eq!(decode_numeric(&numeric_bytes(&[10], -1, 0, 3)), "0.001");
        // 123456789: groups [1, 2345, 6789], weight 2, dscale 0.
        assert_eq!(
            decode_numeric(&numeric_bytes(&[1, 2345, 6789], 2, 0, 0)),
            "123456789"
        );
        // 10000: groups [1], weight 1.
        assert_eq!(decode_numeric(&numeric_bytes(&[1], 1, 0, 0)), "10000");
        // -0.5: groups [5000], weight -1, dscale 1, negative.
        assert_eq!(
            decode_numeric(&numeric_bytes(&[5000], -1, 0x4000, 1)),
            "-0.5"
        );
        // 0.00: no digits, dscale 2.
        assert_eq!(decode_numeric(&numeric_bytes(&[], 0, 0, 2)), "0.00");
        assert_eq!(decode_numeric(&numeric_bytes(&[], 0, 0xC000, 0)), "NaN");
    }

    #[test]
    fn native_field_decoding_preserves_typed_values() {
        let numeric = numeric_bytes(&[61, 7000], 0, 0, 2);
        assert!(
            matches!(decode_cell(&Type::NUMERIC,&numeric),Ok(Cell::Text(value)) if value=="61.70")
        );
        assert_eq!(
            decode_numeric(&numeric_bytes(&[123], -2, 0, 8)),
            "0.00000123"
        );
        let mut timetz = 3_600_000_000i64.to_be_bytes().to_vec();
        timetz.extend(7200i32.to_be_bytes());
        assert!(
            matches!(decode_cell(&Type::TIMETZ,&timetz),Ok(Cell::Text(value)) if value=="01:00:00-02:00")
        );
        let mut interval = 0i64.to_be_bytes().to_vec();
        interval.extend(3i32.to_be_bytes());
        interval.extend(0i32.to_be_bytes());
        assert!(
            matches!(decode_cell(&Type::INTERVAL,&interval),Ok(Cell::Text(value)) if value=="3 days, 0:00:00")
        );
        assert!(decode_cell(&Type::POINT, &[0; 16]).is_err());
    }

    #[test]
    fn interval_rendering() {
        assert_eq!(format_interval(0, 1, 3_661_000_000), "1 day, 1:01:01");
        assert_eq!(format_interval(0, 0, 3_661_000_000), "1:01:01");
        assert_eq!(format_interval(1, 0, 0), "30 days, 0:00:00");
        assert_eq!(format_interval(0, 0, -3_600_000_000), "-1 day, 23:00:00");
        assert_eq!(format_interval(0, 0, 1), "0:00:00.000001");
    }
}
