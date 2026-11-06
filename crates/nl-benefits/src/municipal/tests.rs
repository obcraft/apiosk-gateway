use super::*;

#[tokio::test]
async fn bounded_pdf_extractor_receives_eof_and_returns_text() {
    // The extractor shells out to poppler. A machine without it is not a
    // regression in this crate, so the check is skipped there, out loud.
    if std::process::Command::new("pdftotext").arg("-v").output().is_err() {
        eprintln!("skipping: pdftotext is not installed");
        return;
    }
    let content = "BT /F1 14 Tf 40 200 Td (Municipal benefit application conditions) Tj ET";
    let objects=[
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 300] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_owned(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        format!("<< /Length {} >>\nstream\n{content}\nendstream",content.len()),
    ];
    let mut bytes = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (i, object) in objects.iter().enumerate() {
        offsets.push(bytes.len());
        bytes.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", i + 1).as_bytes());
    }
    let xref = bytes.len();
    bytes.extend_from_slice(b"xref\n0 6\n0000000000 65535 f \n");
    for offset in offsets {
        bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    bytes.extend_from_slice(
        format!("trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n").as_bytes(),
    );
    assert!(pdf(bytes)
        .await
        .unwrap()
        .contains("Municipal benefit application conditions"));
    assert!(pdf(b"not a PDF".to_vec()).await.is_err());
}
#[test]
fn denies_private_and_special_addresses() {
    for ip in [
        "127.0.0.1",
        "10.0.0.1",
        "169.254.169.254",
        "100.64.0.1",
        "192.168.1.1",
        "0.0.0.0",
        "::1",
        "::ffff:127.0.0.1",
        "2001:db8::1",
        "fc00::1",
    ] {
        assert!(!public_ip(ip.parse().unwrap()), "{ip}");
    }
    assert!(public_ip("8.8.8.8".parse().unwrap()));
    for u in [
        "http://gemeente.nl/",
        "https://localhost/",
        "https://gemeente.nl:444/",
        "https://user@gemeente.nl/",
        "https://127.0.0.1/",
    ] {
        assert!(!valid_url(&Url::parse(u).unwrap()));
    }
    let base = Url::parse("https://www.leiderdorp.nl/x").unwrap();
    assert!(redirect(&base, Some("https://evil.nl/a")).is_err());
    assert!(redirect(&base, Some("//127.0.0.1/")).is_err());
    assert!(redirect(&base, Some("/other")).is_ok());
}
#[test]
fn robots_respects_specific_groups_wildcards_encoded_paths_and_ties() {
    let p=Robots::parse("User-agent: *\nDisallow: /\n\nUser-agent: ApioskMunicipalBot\nDisallow: /private\nAllow: /private/open$\nDisallow: /*.pdf$\nCrawl-delay: 2");
    let u = |p| Url::parse(&format!("https://gemeente.nl{p}")).unwrap();
    assert!(p.allows(&u("/regelingen")));
    assert!(!p.allows(&u("/private/a")));
    assert!(p.allows(&u("/private/open")));
    assert!(!p.allows(&u("/a.pdf")));
    assert!(!p.allows(&u("/%70rivate/a")));
    assert_eq!(p.delay, Duration::from_secs(2));
    let p = Robots::parse("User-agent: *\nDisallow: /x\nAllow: /x\n");
    assert!(p.allows(&u("/x")));
}
#[test]
fn extracts_readable_text_and_only_relevant_same_origin_links() {
    let b=br#"<title>Page</title><nav>ignore me</nav><main><h1>Bijzondere bijstand</h1><script>ignore script</script><p>Vraag bijstand aan bij de gemeente.</p><a href='/regelingen/bijstand'>Meer informatie</a><a href='https://evil.nl/bijstand'>Andere site</a></main>"#;
    let (title, text, links, truncated) =
        html(b, &Url::parse("https://gemeente.nl/start").unwrap());
    assert_eq!(title, "Bijzondere bijstand");
    assert!(!text.contains("ignore"));
    assert_eq!(links.len(), 1);
    assert!(!truncated);
}
#[test]
fn catalogue_requires_collection_municipality_authority_and_public_seed() {
    let xml = r#"<searchRetrieveResponse><numberOfRecords>1</numberOfRecords><recordData><creator scheme="overheid:Gemeente">Utrecht</creator><product-area>samenwerkendecatalogi</product-area><title>Bijstand</title><bronIdentifier>https://www.utrecht.nl/bijstand</bronIdentifier></recordData></searchRetrieveResponse>"#;
    assert_eq!(catalogue(xml.as_bytes(), "Utrecht").unwrap().1.len(), 1);
    assert!(catalogue(xml.as_bytes(), "Amsterdam").unwrap().1.is_empty());
    assert!(catalogue(
        xml.replace("samenwerkendecatalogi", "other").as_bytes(),
        "Utrecht"
    )
    .unwrap()
    .1
    .is_empty());
    assert!(catalogue(b"<error/>", "Utrecht").is_err());
    assert_eq!(literal("x\" OR a=b"), "\"x\\\" OR a=b\"");
}
