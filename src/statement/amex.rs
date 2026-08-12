use chrono::NaiveDate;
use serde::Deserialize;

use super::{ImportError, ParsedTransaction, header_contains_all, normalize_description};

const HEADER: &[&str] = &["Date Processed", "Description", "Amount"];
const DATE_FORMAT: &str = "%d %b %Y";

#[derive(Debug, Deserialize)]
struct Row {
    #[serde(rename = "Date Processed")]
    date_processed: String,
    #[serde(rename = "Description")]
    description: String,
    #[serde(rename = "Amount")]
    amount: String,
    /// Present on richer exports (alongside address/foreign-currency
    /// columns this importer otherwise ignores) — absent on the simpler
    /// `activity.csv` shape, hence `default` rather than a required field.
    #[serde(rename = "Merchant", default)]
    merchant: Option<String>,
}

/// Amex `activity.csv` export. Dates are `07 Aug 2026`, and — unlike Neo —
/// a purchase is already a *positive* amount, matching Plaid's convention,
/// so no sign flip is needed here.
pub struct AmexImporter;

impl super::StatementImporter for AmexImporter {
    fn detect(&self, header: &csv::StringRecord) -> bool {
        header_contains_all(header, HEADER)
    }

    fn parse(&self, bytes: &[u8]) -> Result<Vec<ParsedTransaction>, ImportError> {
        let mut reader = csv::ReaderBuilder::new().from_reader(bytes);
        let mut out = Vec::new();

        for (i, result) in reader.deserialize::<Row>().enumerate() {
            let row = result?;

            let date =
                NaiveDate::parse_from_str(&row.date_processed, DATE_FORMAT).map_err(|source| {
                    ImportError::InvalidDate {
                        row: i + 1,
                        value: row.date_processed.clone(),
                        source,
                    }
                })?;
            let amount: f64 = row
                .amount
                .parse()
                .map_err(|source| ImportError::InvalidAmount {
                    row: i + 1,
                    value: row.amount.clone(),
                    source,
                })?;

            let description = normalize_description(&row.description);
            let merchant = row
                .merchant
                .as_deref()
                .map(normalize_description)
                .unwrap_or_else(|| description.clone());
            out.push(ParsedTransaction {
                date,
                amount,
                description,
                merchant,
            });
        }

        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::statement::StatementImporter;

    const SAMPLE: &str = "Date,Date Processed,Description,Amount\n\
07 Aug 2026,07 Aug 2026,MEMBERSHIP FEE INSTALLMENT,15.99\n\
04 Aug 2026,06 Aug 2026,MCDONALD'S #6261        WETASKIWIN,12.36\n\
04 Aug 2026,06 Aug 2026,WETASKIWIN ESSO OTR     WETASKIWIN,54.98\n\
03 Aug 2026,04 Aug 2026,MOBIL@ 3825 GAS STN     WESTLOCK,34.68\n\
02 Aug 2026,03 Aug 2026,CITY OF EDM TICKETS EDM EDMONTON,75.00\n\
02 Aug 2026,04 Aug 2026,MCDONALD'S #5634   QPS  SHERWOOD PARK,11.94\n\
01 Aug 2026,01 Aug 2026,A LITTLE BIT OF EVERYTH Athabasca,6.30\n\
31 Jul 2026,01 Aug 2026,FGP40019 CLYDE CORNER F WESTLOCK COUN,70.59\n";

    #[test]
    fn parses_the_real_sample_export() {
        let importer = AmexImporter;
        let rows = importer.parse(SAMPLE.as_bytes()).unwrap();
        assert_eq!(rows.len(), 8);

        // Canonical date is "Date Processed", not "Date" — the two differ
        // on several rows in this sample (e.g. row 2: 04 Aug vs 06 Aug).
        assert_eq!(
            rows[0],
            ParsedTransaction {
                date: NaiveDate::from_ymd_opt(2026, 8, 7).unwrap(),
                amount: 15.99,
                description: "MEMBERSHIP FEE INSTALLMENT".to_string(),
                merchant: "MEMBERSHIP FEE INSTALLMENT".to_string(),
            }
        );
        assert_eq!(rows[1].date, NaiveDate::from_ymd_opt(2026, 8, 6).unwrap());
        assert_eq!(rows[1].description, "MCDONALD'S #6261 WETASKIWIN");

        // Last row crosses a month boundary (31 Jul processed 01 Aug).
        assert_eq!(rows[7].date, NaiveDate::from_ymd_opt(2026, 8, 1).unwrap());
    }

    #[test]
    fn amount_sign_already_matches_plaids_convention() {
        let rows = AmexImporter.parse(SAMPLE.as_bytes()).unwrap();
        assert!(
            rows.iter().all(|r| r.amount > 0.0),
            "every row in the sample is a purchase"
        );
    }

    #[test]
    fn merchant_falls_back_to_description_when_theres_no_merchant_column() {
        let rows = AmexImporter.parse(SAMPLE.as_bytes()).unwrap();
        assert!(rows.iter().all(|r| r.merchant == r.description));
    }

    #[test]
    fn detects_own_header() {
        let mut reader = csv::ReaderBuilder::new()
            .from_reader("Date,Date Processed,Description,Amount\n".as_bytes());
        assert!(AmexImporter.detect(reader.headers().unwrap()));
    }

    // A richer export shape (loyalty/address/foreign-currency columns this
    // importer ignores, plus a genuine `Merchant` column) that showed up
    // later — this importer must still detect and parse it even though its
    // header is a strict superset of, and reordered relative to, `SAMPLE`'s.
    const RICH_SAMPLE: &str = "\
Date,Date Processed,Description,Amount,Foreign Spend Amount,Commission,Exchange Rate,Additional Information,Merchant,Address,City / Province,Postal Code,Country,Reference\n\
07 Jul 2026,07 Jul 2026,MEMBERSHIP FEE INSTALLMENT,15.99,,,,,MEMBERSHIP FEE INSTALLMENT,,,,,'10000000010407999794240'\n\
06 Jul 2026,07 Jul 2026,FRIENDS CAFE YYC 00-080 CALGARY,12.68,,,,,FRIENDS CAFE YYC 00-080 CALGARY,\"45 EDENWOLD DR\nNORTHWEST\",\"CALGARY\nAB\",T3A 3S8,CANADA,'AT261880006000010258921'\n\
06 Jul 2026,06 Jul 2026,PAYMENT RECEIVED - THANK YOU,-1500.00,,,,,PAYMENT RECEIVED - THANK YOU,,,,,'AT261870003000010050122'\n\
05 Jul 2026,06 Jul 2026,EDGEMONT ATHLETIC 41158 CALGARY,26.25,,,,,EDGEMONT ATHLETIC 41158 CALGARY,\"7222 EDGEMONT BLVD\nNORTHWEST\",\"CALGARY\nAB\",T3A 2X7,CANADA,'AT261870006000010215424'\n";

    #[test]
    fn detects_a_richer_export_with_extra_columns() {
        let mut reader = csv::ReaderBuilder::new().from_reader(RICH_SAMPLE.as_bytes());
        assert!(AmexImporter.detect(reader.headers().unwrap()));
    }

    #[test]
    fn parses_a_richer_export_and_extracts_its_merchant_column() {
        let rows = AmexImporter.parse(RICH_SAMPLE.as_bytes()).unwrap();
        assert_eq!(rows.len(), 4);

        assert_eq!(rows[0].merchant, "MEMBERSHIP FEE INSTALLMENT");
        assert_eq!(rows[0].amount, 15.99);

        // A credit/payment is already negative, same convention as before.
        assert_eq!(rows[2].description, "PAYMENT RECEIVED - THANK YOU");
        assert_eq!(rows[2].amount, -1500.00);

        // The multi-line quoted Address/City fields don't leak into the
        // columns this importer actually reads.
        assert_eq!(rows[1].description, "FRIENDS CAFE YYC 00-080 CALGARY");
        assert_eq!(rows[1].date, NaiveDate::from_ymd_opt(2026, 7, 7).unwrap());
    }
}
