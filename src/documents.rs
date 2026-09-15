//! Document reading for workflows (today: invoices). The browser reads a PDF
//! text layer or one page image itself, and then nothing is stored. Only when
//! reading requires the original file (multi-page scans, files the browser
//! cannot open) is it kept in `gateway_v2.document_uploads`, for at most one
//! hour, and deleted as soon as it has been read.
use base64::Engine as _;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use uuid::Uuid;

pub const MAX_TEXT: usize = 60_000;
pub const MAX_IMAGE_URL: usize = 3 * 1024 * 1024;
pub const MAX_FILE_BYTES: usize = 10 * 1024 * 1024;
pub const PURPOSES: &[&str] = &["invoice"];

pub enum Content<'a> {
    Text(&'a str),
    Image(&'a str),
    File { file_name: &'a str, data_url: String },
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentRequest {
    pub request_id: Uuid,
    pub purpose: String,
    pub file_name: String,
    #[serde(default)]
    pub mime_type: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub image_data_url: Option<String>,
    #[serde(default)]
    pub file_base64: Option<String>,
    #[serde(default)]
    pub language: Option<String>,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReadAgain {
    pub request_id: Uuid,
    #[serde(default)]
    pub language: Option<String>,
}

pub enum Reading {
    Text(String),
    Image(String),
    File { mime_type: &'static str, bytes: Vec<u8> },
}

fn sniff(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"%PDF-") {
        Some("application/pdf")
    } else if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        Some("image/png")
    } else if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else {
        None
    }
}

impl DocumentRequest {
    pub fn dutch(&self) -> bool {
        self.language.as_deref() != Some("en")
    }
    /// Exactly one bounded form; a file must really be the PDF/PNG/JPEG it claims.
    pub fn reading(&self) -> Result<Reading, &'static str> {
        if !PURPOSES.contains(&self.purpose.as_str()) {
            return Err("Unsupported document purpose.");
        }
        if self.file_name.is_empty() || self.file_name.len() > 255 || self.file_name.chars().any(char::is_control) {
            return Err("Invalid file name.");
        }
        match (&self.text, &self.image_data_url, &self.file_base64) {
            (Some(text), None, None) if !text.trim().is_empty() && text.len() <= MAX_TEXT => Ok(Reading::Text(text.clone())),
            (None, Some(image), None) if image.len() <= MAX_IMAGE_URL && (image.starts_with("data:image/jpeg;base64,") || image.starts_with("data:image/png;base64,")) => Ok(Reading::Image(image.clone())),
            (None, None, Some(encoded)) if encoded.len() <= MAX_FILE_BYTES.div_ceil(3) * 4 + 4 => {
                let bytes = base64::engine::general_purpose::STANDARD.decode(encoded.trim()).map_err(|_| "The file is not valid base64.")?;
                let mime_type = sniff(&bytes).ok_or("Only PDF, PNG and JPEG files can be read.")?;
                if bytes.len() > MAX_FILE_BYTES {
                    return Err("The file is larger than 10 MB.");
                }
                if self.mime_type.as_deref().is_some_and(|m| m != mime_type) {
                    return Err("The file type does not match its contents.");
                }
                Ok(Reading::File { mime_type, bytes })
            }
            _ => Err("Send exactly one of: document text (max 60,000 characters), one JPEG/PNG page image (max 3 MB), or the original PDF/PNG/JPEG file (max 10 MB)."),
        }
    }
}

pub struct Stored {
    pub id: Uuid,
    pub file_name: String,
    pub mime_type: String,
    pub bytes: Vec<u8>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

pub async fn store(pool: &PgPool, owner: Uuid, workspace: Option<Uuid>, purpose: &str, file_name: &str, mime_type: &str, bytes: &[u8]) -> anyhow::Result<Stored> {
    let id = Uuid::new_v4();
    let expires_at: chrono::DateTime<chrono::Utc> = sqlx::query_scalar("INSERT INTO document_uploads(id,owner_id,workspace_id,purpose,file_name,mime_type,sha256,byte_size,content) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9) RETURNING expires_at")
        .bind(id).bind(owner).bind(workspace).bind(purpose).bind(file_name).bind(mime_type)
        .bind(hex::encode(Sha256::digest(bytes))).bind(bytes.len() as i32).bind(bytes)
        .fetch_one(pool).await?;
    Ok(Stored { id, file_name: file_name.to_owned(), mime_type: mime_type.to_owned(), bytes: bytes.to_vec(), expires_at })
}
pub async fn load(pool: &PgPool, owner: Uuid, workspace: Option<Uuid>, id: Uuid) -> anyhow::Result<Option<Stored>> {
    Ok(sqlx::query("SELECT file_name,mime_type,content,expires_at FROM document_uploads WHERE id=$1 AND owner_id=$2 AND workspace_id IS NOT DISTINCT FROM $3 AND expires_at>now()")
        .bind(id).bind(owner).bind(workspace).fetch_optional(pool).await?
        .map(|row| Stored { id, file_name: row.get("file_name"), mime_type: row.get("mime_type"), bytes: row.get("content"), expires_at: row.get("expires_at") }))
}
pub async fn delete(pool: &PgPool, owner: Uuid, id: Uuid) -> anyhow::Result<()> {
    sqlx::query("DELETE FROM document_uploads WHERE id=$1 AND owner_id=$2").bind(id).bind(owner).execute(pool).await?;
    Ok(())
}
pub fn data_url(mime_type: &str, bytes: &[u8]) -> String {
    format!("data:{mime_type};base64,{}", base64::engine::general_purpose::STANDARD.encode(bytes))
}

/// The reviewable result of reading an invoice.
pub fn invoice_result(raw: &Value, text: Option<&str>, reading: &str, dutch: bool, document: Value) -> Result<Value, &'static str> {
    use crate::invoice_document::{question, verify};
    let (fields, removed) = verify(raw, text);
    if fields.as_object().is_some_and(|f| f.values().all(Value::is_null)) {
        return Err("No supplier details were found in this document.");
    }
    let mut warnings: Vec<String> = removed.iter().map(|field| if dutch { format!("{field} stond niet letterlijk in de tekst en is weggelaten.") } else { format!("{field} was not printed literally in the text and was left out.") }).collect();
    if text.is_none() {
        warnings.push(if dutch { "Gelezen uit een afbeelding of scan: controleer de gegevens voordat je verder gaat.".into() } else { "Read from an image or scan: check the details before continuing.".into() });
    }
    if fields["iban"].as_str().is_some_and(|iban| !crate::supplier_check::iban_valid(&iban.chars().filter(char::is_ascii_alphanumeric).collect::<String>())) {
        warnings.push(if dutch { "Het IBAN op de factuur heeft een ongeldig controlegetal.".into() } else { "The IBAN on the invoice has invalid check digits.".into() });
    }
    Ok(json!({"purpose":"invoice","reading":reading,"fields":fields,"question":question(&fields, dutch),"verified_in_text":text.is_some(),"warnings":warnings,"document":document}))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(text: Option<&str>, image: Option<&str>, file: Option<&str>) -> DocumentRequest {
        DocumentRequest { request_id: Uuid::new_v4(), purpose: "invoice".into(), file_name: "invoice.pdf".into(), mime_type: None, text: text.map(Into::into), image_data_url: image.map(Into::into), file_base64: file.map(Into::into), language: None }
    }
    #[test]
    fn accepts_exactly_one_real_document_form() {
        assert!(matches!(request(Some("Factuur"), None, None).reading(), Ok(Reading::Text(_))));
        assert!(matches!(request(None, Some("data:image/png;base64,AA"), None).reading(), Ok(Reading::Image(_))));
        let pdf = base64::engine::general_purpose::STANDARD.encode(b"%PDF-1.7\n%%EOF");
        assert!(matches!(request(None, None, Some(&pdf)).reading(), Ok(Reading::File { mime_type: "application/pdf", .. })));
        let html = base64::engine::general_purpose::STANDARD.encode(b"<html><script>");
        assert!(request(None, None, Some(&html)).reading().is_err());
        assert!(request(Some("x"), None, Some(&pdf)).reading().is_err());
        assert!(request(None, Some("data:image/svg+xml;base64,AA"), None).reading().is_err());
        let mut mismatch = request(None, None, Some(&pdf));
        mismatch.mime_type = Some("image/png".into());
        assert!(mismatch.reading().is_err());
        let mut other = request(Some("x"), None, None);
        other.purpose = "contract".into();
        assert!(other.reading().is_err());
    }
}
