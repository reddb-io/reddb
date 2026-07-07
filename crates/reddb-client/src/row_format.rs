use crate::{QueryResult, ValueOut};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowFormat {
    Table,
    Json,
    Ndjson,
    Csv,
    Tsv,
    Toon,
}

impl RowFormat {
    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "table" => Some(Self::Table),
            "json" => Some(Self::Json),
            "ndjson" => Some(Self::Ndjson),
            "csv" => Some(Self::Csv),
            "tsv" => Some(Self::Tsv),
            "toon" => Some(Self::Toon),
            _ => None,
        }
    }

    pub fn vocabulary() -> &'static str {
        "table, json, ndjson, csv, tsv, toon"
    }
}

pub fn format_query_result(result: &QueryResult, format: RowFormat) -> Vec<u8> {
    match format {
        RowFormat::Table => format_table(result).into_bytes(),
        RowFormat::Json => format_json(result).into_bytes(),
        RowFormat::Ndjson => format_ndjson(result).into_bytes(),
        RowFormat::Csv => format_delimited(result, b','),
        RowFormat::Tsv => format_delimited(result, b'\t'),
        RowFormat::Toon => format_toon(result).into_bytes(),
    }
}

fn columns(result: &QueryResult) -> Vec<String> {
    if !result.columns.is_empty() {
        return result.columns.clone();
    }
    result
        .rows
        .first()
        .map(|row| row.iter().map(|(key, _)| key.clone()).collect())
        .unwrap_or_default()
}

fn value_at<'a>(row: &'a [(String, ValueOut)], column: &str) -> Option<&'a ValueOut> {
    row.iter()
        .find(|(key, _)| key == column)
        .map(|(_, value)| value)
}

fn format_table(result: &QueryResult) -> String {
    let columns = columns(result);
    if result.rows.is_empty() {
        return "(no rows)\n".to_string();
    }

    let mut widths: Vec<usize> = columns.iter().map(|column| column.len()).collect();
    for row in &result.rows {
        for (index, column) in columns.iter().enumerate() {
            let value = value_at(row, column)
                .map(table_value)
                .unwrap_or_else(|| "null".to_string());
            widths[index] = widths[index].max(value.len());
        }
    }

    let mut out = String::new();
    write_table_line(&mut out, &columns, &widths);
    let separator: Vec<String> = widths.iter().map(|width| "-".repeat(*width)).collect();
    write_table_line(&mut out, &separator, &widths);
    for row in &result.rows {
        let values: Vec<String> = columns
            .iter()
            .map(|column| {
                value_at(row, column)
                    .map(table_value)
                    .unwrap_or_else(|| "null".to_string())
            })
            .collect();
        write_table_line(&mut out, &values, &widths);
    }
    out
}

fn write_table_line(out: &mut String, cells: &[String], widths: &[usize]) {
    for (index, cell) in cells.iter().enumerate() {
        if index > 0 {
            out.push_str("  ");
        }
        out.push_str(cell);
        if index + 1 < cells.len() {
            for _ in cell.len()..widths[index] {
                out.push(' ');
            }
        }
    }
    out.push('\n');
}

fn format_json(result: &QueryResult) -> String {
    let columns = columns(result);
    let mut out = String::from("[");
    for (row_index, row) in result.rows.iter().enumerate() {
        if row_index > 0 {
            out.push(',');
        }
        write_json_row(&mut out, row, &columns);
    }
    out.push_str("]\n");
    out
}

fn format_ndjson(result: &QueryResult) -> String {
    let columns = columns(result);
    let mut out = String::new();
    for row in &result.rows {
        write_json_row(&mut out, row, &columns);
        out.push('\n');
    }
    out
}

fn format_toon(result: &QueryResult) -> String {
    let columns = columns(result);
    let mut out = String::new();
    out.push('[');
    out.push_str(&result.rows.len().to_string());
    out.push_str("]{");
    for (index, column) in columns.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        write_toon_key(&mut out, column);
    }
    out.push_str("}:\n");
    for row in &result.rows {
        out.push_str("  ");
        for (index, column) in columns.iter().enumerate() {
            if index > 0 {
                out.push(',');
            }
            if let Some(value) = value_at(row, column) {
                write_toon_value(&mut out, value);
            } else {
                out.push_str("null");
            }
        }
        out.push('\n');
    }
    out
}

fn write_json_row(out: &mut String, row: &[(String, ValueOut)], columns: &[String]) {
    out.push('{');
    for (index, column) in columns.iter().enumerate() {
        if index > 0 {
            out.push(',');
        }
        write_json_string(out, column);
        out.push(':');
        if let Some(value) = value_at(row, column) {
            write_json_value(out, value);
        } else {
            out.push_str("null");
        }
    }
    out.push('}');
}

fn write_toon_key(out: &mut String, value: &str) {
    if is_toon_bare_key(value) {
        out.push_str(value);
    } else {
        write_toon_string(out, value);
    }
}

fn write_toon_value(out: &mut String, value: &ValueOut) {
    match value {
        ValueOut::Null => out.push_str("null"),
        ValueOut::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
        ValueOut::Integer(value) => out.push_str(&value.to_string()),
        ValueOut::Float(value) => out.push_str(&value.to_string()),
        ValueOut::String(value) => {
            if is_toon_bare_string(value) {
                out.push_str(value);
            } else {
                write_toon_string(out, value);
            }
        }
    }
}

fn is_toon_bare_key(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first == '_' || first.is_ascii_alphabetic()) {
        return false;
    }
    chars.all(|c| c == '_' || c == '-' || c == '.' || c.is_ascii_alphanumeric())
}

fn is_toon_bare_string(value: &str) -> bool {
    !value.is_empty()
        && !matches!(value, "true" | "false" | "null")
        && value.parse::<f64>().is_err()
        && !value.starts_with('-')
        && !value.chars().any(|c| {
            matches!(
                c,
                ',' | ':' | '{' | '}' | '[' | ']' | '"' | '\\' | '\n' | '\r' | '\t'
            )
        })
}

fn write_toon_string(out: &mut String, value: &str) {
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

fn format_delimited(result: &QueryResult, delimiter: u8) -> Vec<u8> {
    let columns = columns(result);
    let mut out = Vec::new();
    write_delimited_record(&mut out, &columns, delimiter);
    for row in &result.rows {
        let values: Vec<String> = columns
            .iter()
            .map(|column| value_at(row, column).map(raw_value).unwrap_or_default())
            .collect();
        write_delimited_record(&mut out, &values, delimiter);
    }
    out
}

fn write_delimited_record(out: &mut Vec<u8>, fields: &[String], delimiter: u8) {
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            out.push(delimiter);
        }
        write_delimited_field(out, field, delimiter);
    }
    out.push(b'\n');
}

fn write_delimited_field(out: &mut Vec<u8>, field: &str, delimiter: u8) {
    let needs_quotes = field
        .bytes()
        .any(|byte| byte == delimiter || byte == b'"' || byte == b'\n' || byte == b'\r');
    if !needs_quotes {
        out.extend_from_slice(field.as_bytes());
        return;
    }
    out.push(b'"');
    for byte in field.bytes() {
        if byte == b'"' {
            out.extend_from_slice(b"\"\"");
        } else {
            out.push(byte);
        }
    }
    out.push(b'"');
}

fn table_value(value: &ValueOut) -> String {
    raw_value(value)
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('\r', "\\r")
        .replace('\t', "\\t")
}

fn raw_value(value: &ValueOut) -> String {
    match value {
        ValueOut::Null => String::new(),
        ValueOut::Bool(value) => value.to_string(),
        ValueOut::Integer(value) => value.to_string(),
        ValueOut::Float(value) => value.to_string(),
        ValueOut::String(value) => value.clone(),
    }
}

fn write_json_value(out: &mut String, value: &ValueOut) {
    match value {
        ValueOut::Null => out.push_str("null"),
        ValueOut::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
        ValueOut::Integer(value) => out.push_str(&value.to_string()),
        ValueOut::Float(value) => out.push_str(&value.to_string()),
        ValueOut::String(value) => write_json_string(out, value),
    }
}

fn write_json_string(out: &mut String, value: &str) {
    out.push('"');
    for c in value.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> QueryResult {
        QueryResult {
            statement: "select".to_string(),
            affected: 0,
            columns: vec!["id".to_string(), "name".to_string(), "note".to_string()],
            rows: vec![
                vec![
                    ("id".to_string(), ValueOut::Integer(1)),
                    ("name".to_string(), ValueOut::String("Ada".to_string())),
                    (
                        "note".to_string(),
                        ValueOut::String("quote \" comma ,".to_string()),
                    ),
                ],
                vec![
                    ("id".to_string(), ValueOut::Integer(2)),
                    ("name".to_string(), ValueOut::String("Linus".to_string())),
                    (
                        "note".to_string(),
                        ValueOut::String("tab\tline\nend".to_string()),
                    ),
                ],
            ],
        }
    }

    #[test]
    fn formats_table_byte_exact() {
        assert_eq!(
            format_query_result(&sample(), RowFormat::Table),
            b"id  name   note\n--  -----  ---------------\n1   Ada    quote \" comma ,\n2   Linus  tab\\tline\\nend\n"
        );
    }

    #[test]
    fn formats_json_byte_exact() {
        assert_eq!(
            format_query_result(&sample(), RowFormat::Json),
            br#"[{"id":1,"name":"Ada","note":"quote \" comma ,"},{"id":2,"name":"Linus","note":"tab\tline\nend"}]
"#
        );
    }

    #[test]
    fn formats_ndjson_one_object_per_row() {
        assert_eq!(
            format_query_result(&sample(), RowFormat::Ndjson),
            br#"{"id":1,"name":"Ada","note":"quote \" comma ,"}
{"id":2,"name":"Linus","note":"tab\tline\nend"}
"#
        );
    }

    #[test]
    fn formats_csv_byte_exact() {
        assert_eq!(
            format_query_result(&sample(), RowFormat::Csv),
            b"id,name,note\n1,Ada,\"quote \"\" comma ,\"\n2,Linus,\"tab\tline\nend\"\n"
        );
    }

    #[test]
    fn formats_tsv_byte_exact() {
        assert_eq!(
            format_query_result(&sample(), RowFormat::Tsv),
            b"id\tname\tnote\n1\tAda\t\"quote \"\" comma ,\"\n2\tLinus\t\"tab\tline\nend\"\n"
        );
    }

    #[test]
    fn formats_toon_byte_exact() {
        assert_eq!(
            format_query_result(&sample(), RowFormat::Toon),
            br#"[2]{id,name,note}:
  1,Ada,"quote \" comma ,"
  2,Linus,"tab\tline\nend"
"#
        );
    }

    #[test]
    fn formats_toon_quotes_delimiters_and_scalar_like_strings() {
        let result = QueryResult {
            statement: "select".to_string(),
            affected: 0,
            columns: vec![
                "label".to_string(),
                "empty".to_string(),
                "dash".to_string(),
                "truthy".to_string(),
                "nullable".to_string(),
                "count".to_string(),
                "enabled".to_string(),
            ],
            rows: vec![vec![
                ("label".to_string(), ValueOut::String("a,b".to_string())),
                ("empty".to_string(), ValueOut::String(String::new())),
                ("dash".to_string(), ValueOut::String("-x".to_string())),
                ("truthy".to_string(), ValueOut::String("true".to_string())),
                ("nullable".to_string(), ValueOut::Null),
                ("count".to_string(), ValueOut::Integer(7)),
                ("enabled".to_string(), ValueOut::Bool(false)),
            ]],
        };

        assert_eq!(
            format_query_result(&result, RowFormat::Toon),
            br#"[1]{label,empty,dash,truthy,nullable,count,enabled}:
  "a,b","","-x","true",null,7,false
"#
        );
    }

    #[test]
    fn csv_and_tsv_round_trip_through_standard_parser() {
        for (format, delimiter) in [(RowFormat::Csv, b','), (RowFormat::Tsv, b'\t')] {
            let bytes = format_query_result(&sample(), format);
            let mut reader = csv::ReaderBuilder::new()
                .delimiter(delimiter)
                .from_reader(bytes.as_slice());
            let records: Vec<csv::StringRecord> =
                reader.records().collect::<Result<_, _>>().unwrap();
            assert_eq!(reader.headers().unwrap(), &["id", "name", "note"][..]);
            assert_eq!(records[0].get(2), Some("quote \" comma ,"));
            assert_eq!(records[1].get(2), Some("tab\tline\nend"));
        }
    }
}
