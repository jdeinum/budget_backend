use chrono::NaiveDate;
use serde::Deserialize;

use super::{ImportError, ParsedTransaction, header_contains_all, normalize_description};

const HEADER: &[&str] = &["Posted Date", "Status", "Description", "Amount"];
const DATE_FORMAT: &str = "%Y-%m-%d";

#[derive(Debug, Deserialize)]
struct Row {
    #[serde(rename = "Posted Date")]
    posted_date: String,
    #[serde(rename = "Status")]
    status: String,
    #[serde(rename = "Description")]
    description: String,
    #[serde(rename = "Amount")]
    amount: String,
}

/// Neo World Elite Mastercard CSV export. Dates are ISO (`2026-08-10`) and,
/// unlike Plaid, a purchase is a *negative* amount — so amounts are negated
/// on the way in to match Plaid's convention (positive = money out).
pub struct NeoImporter;

impl super::StatementImporter for NeoImporter {
    fn detect(&self, header: &csv::StringRecord) -> bool {
        header_contains_all(header, HEADER)
    }

    fn parse(&self, bytes: &[u8]) -> Result<Vec<ParsedTransaction>, ImportError> {
        let mut reader = csv::ReaderBuilder::new().from_reader(bytes);
        let mut out = Vec::new();

        for (i, result) in reader.deserialize::<Row>().enumerate() {
            let row = result?;
            // Pending Neo transactions can still change amount or date
            // before they post, so importing them risks a duplicate once
            // the posted version shows up in a later export — skip them.
            if row.status != "Posted" {
                continue;
            }

            let date = NaiveDate::parse_from_str(&row.posted_date, DATE_FORMAT).map_err(|source| {
                ImportError::InvalidDate { row: i + 1, value: row.posted_date.clone(), source }
            })?;
            let raw_amount: f64 = row.amount.parse().map_err(|source| ImportError::InvalidAmount {
                row: i + 1,
                value: row.amount.clone(),
                source,
            })?;

            let description = normalize_description(&row.description);
            out.push(ParsedTransaction {
                date,
                amount: -raw_amount,
                // Neo's export has no separate merchant field — the
                // description ("REAL CDN LIQUOR STR 16...") is the closest
                // thing to one, so it doubles as both.
                merchant: description.clone(),
                description,
            });
        }

        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::statement::StatementImporter;

    const SAMPLE: &str = "Transaction Date,Posted Date,Status,Description,Amount\n\
2026-08-10,2026-08-11,Posted,REAL CDN LIQUOR STR 16 CALGARY       CAN,-26.35\n\
2026-08-10,2026-08-11,Posted,REAL CDN SUPERSTORE #1 CALGARY       CAN,-37.04\n\
2026-08-09,2026-08-10,Posted,CANCO PETROLEUM #219 E EDMONTON      CAN,-69.25\n\
2026-08-08,2026-08-09,Posted,REAL CANADIAN LIQUOR # EDMONTON      CAN,-13.20\n\
2026-08-08,2026-08-09,Posted,REAL CDN SUPERSTORE #1 EDMONTON      CAN,-22.67\n\
2026-08-08,2026-08-09,Posted,VALUE VILLAGE # 2085   EDMONTON      CAN,-20.99\n\
2026-08-08,2026-08-09,Posted,TELUS MOBILITY         EDMONTON      CAN,-35.97\n\
2026-08-08,2026-08-08,Posted,Annual Card Fee,-149.00\n";

    #[test]
    fn parses_the_real_sample_export() {
        let importer = NeoImporter;
        let rows = importer.parse(SAMPLE.as_bytes()).unwrap();
        assert_eq!(rows.len(), 8);

        assert_eq!(
            rows[0],
            ParsedTransaction {
                date: NaiveDate::from_ymd_opt(2026, 8, 11).unwrap(),
                amount: 26.35,
                description: "REAL CDN LIQUOR STR 16 CALGARY CAN".to_string(),
                merchant: "REAL CDN LIQUOR STR 16 CALGARY CAN".to_string(),
            }
        );
        assert_eq!(rows[7].description, "Annual Card Fee");
        assert_eq!(rows[7].amount, 149.00);
    }

    #[test]
    fn negates_amount_to_match_plaids_convention() {
        let importer = NeoImporter;
        let rows = importer.parse(SAMPLE.as_bytes()).unwrap();
        assert!(rows.iter().all(|r| r.amount > 0.0), "every row in the sample is a purchase");
    }

    #[test]
    fn merchant_falls_back_to_description() {
        let rows = NeoImporter.parse(SAMPLE.as_bytes()).unwrap();
        assert!(rows.iter().all(|r| r.merchant == r.description));
    }

    #[test]
    fn skips_pending_rows() {
        let csv = "Transaction Date,Posted Date,Status,Description,Amount\n\
2026-08-11,2026-08-11,Pending,SOME MERCHANT,-10.00\n\
2026-08-10,2026-08-11,Posted,OTHER MERCHANT,-20.00\n";
        let rows = NeoImporter.parse(csv.as_bytes()).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].description, "OTHER MERCHANT");
    }

    #[test]
    fn detects_own_header() {
        let mut reader = csv::ReaderBuilder::new()
            .from_reader("Transaction Date,Posted Date,Status,Description,Amount\n".as_bytes());
        assert!(NeoImporter.detect(reader.headers().unwrap()));
    }
}
