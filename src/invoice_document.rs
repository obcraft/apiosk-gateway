//! The invoice a supplier payment check reads: the fields its workflow form
//! offers, and the largest file its upload input advertises.

pub const FIELDS: &[&str] = &["supplier_name", "kvk_number", "company_number_uk", "vat_number", "iban", "website", "email", "street", "postcode", "city", "country", "invoice_number", "invoice_date", "total_amount", "currency"];

pub const MAX_FILE_BYTES: usize = 10 * 1024 * 1024;
