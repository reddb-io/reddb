//! Parser for CREATE/DROP TIMESERIES

use super::super::ast::{CreateTimeSeriesQuery, DropTimeSeriesQuery, QueryExpr};
use super::super::lexer::Token;
use super::error::ParseError;
use super::Parser;

impl<'a> Parser<'a> {
    /// Parse CREATE TIMESERIES body (after CREATE TIMESERIES consumed)
    pub fn parse_create_timeseries_body(&mut self) -> Result<QueryExpr, ParseError> {
        let if_not_exists = self.match_if_not_exists()?;
        let name = self.expect_ident()?;

        let mut retention_ms = None;
        let mut chunk_size = None;
        let mut downsample_policies = Vec::new();

        // Parse optional clauses in any order
        loop {
            if self.consume(&Token::Retention)? {
                let value = self.parse_float()?;
                let unit = self.parse_duration_unit()?;
                retention_ms = Some((value * unit) as u64);
            } else if self.consume_ident_ci("CHUNK_SIZE")? || self.consume_ident_ci("CHUNKSIZE")? {
                chunk_size = Some(self.parse_integer()? as usize);
            } else if self.consume_ident_ci("DOWNSAMPLE")? {
                downsample_policies.push(self.parse_downsample_policy_spec()?);
                while self.consume(&Token::Comma)? {
                    downsample_policies.push(self.parse_downsample_policy_spec()?);
                }
            } else {
                break;
            }
        }

        Ok(QueryExpr::CreateTimeSeries(CreateTimeSeriesQuery {
            name,
            retention_ms,
            chunk_size,
            downsample_policies,
            if_not_exists,
        }))
    }

    /// Parse DROP TIMESERIES body (after DROP TIMESERIES consumed)
    pub fn parse_drop_timeseries_body(&mut self) -> Result<QueryExpr, ParseError> {
        let if_exists = self.match_if_exists()?;
        let name = self.expect_ident()?;
        Ok(QueryExpr::DropTimeSeries(DropTimeSeriesQuery {
            name,
            if_exists,
        }))
    }

    /// Parse a duration unit and return the multiplier in milliseconds
    fn parse_duration_unit(&mut self) -> Result<f64, ParseError> {
        match self.peek().clone() {
            Token::Ident(ref unit) => {
                let mult = match unit.to_ascii_lowercase().as_str() {
                    "ms" | "msec" | "millisecond" | "milliseconds" => 1.0,
                    "s" | "sec" | "secs" | "second" | "seconds" => 1_000.0,
                    "m" | "min" | "mins" | "minute" | "minutes" => 60_000.0,
                    "h" | "hr" | "hrs" | "hour" | "hours" => 3_600_000.0,
                    "d" | "day" | "days" => 86_400_000.0,
                    other => {
                        return Err(ParseError::new(
                            format!("unknown duration unit '{}', expected s/m/h/d", other),
                            self.position(),
                        ));
                    }
                };
                self.advance()?;
                Ok(mult)
            }
            _ => Ok(1_000.0), // default: seconds
        }
    }

    fn parse_downsample_policy_spec(&mut self) -> Result<String, ParseError> {
        let target = self.parse_resolution_spec()?;
        self.expect(Token::Colon)?;
        let source = self.parse_resolution_spec()?;
        let aggregation = if self.consume(&Token::Colon)? {
            self.expect_ident_or_keyword()?.to_ascii_lowercase()
        } else {
            "avg".to_string()
        };
        Ok(format!("{target}:{source}:{aggregation}"))
    }

    fn parse_resolution_spec(&mut self) -> Result<String, ParseError> {
        match self.peek().clone() {
            Token::Ident(value) if value.eq_ignore_ascii_case("raw") => {
                self.advance()?;
                Ok(value.to_ascii_lowercase())
            }
            Token::Integer(value) => {
                self.advance()?;
                let unit = self.expect_ident_or_keyword()?.to_ascii_lowercase();
                Ok(format!("{value}{unit}"))
            }
            Token::Float(value) => {
                self.advance()?;
                let unit = self.expect_ident_or_keyword()?.to_ascii_lowercase();
                let number = if value.fract().abs() < f64::EPSILON {
                    format!("{}", value as i64)
                } else {
                    value.to_string()
                };
                Ok(format!("{number}{unit}"))
            }
            other => Err(ParseError::new(
                format!(
                    "expected duration literal for downsample policy, got {}",
                    other
                ),
                self.position(),
            )),
        }
    }
}
