//! Multilingual calibration: questions in Dutch, French, German, Spanish and
//! English, about company registers, company officers, statistics
//! (unemployment, inflation, population, bankruptcies), tenders, addresses,
//! VAT numbers and news, against a directory whose listings are written in
//! their publishers' languages, as the live catalog's CBS, KVK, overheid and
//! Sirenic listings are.
//!
//! Each question is parsed as the Ask parse prompt now requires, into an
//! English capability object whatever the question's language (the subject
//! keeps the person's own wording). The listings are staged the way imports
//! arrive (a generated `operation.*` id, the publisher's own description),
//! and the fake translator returns the English a faithful translation gives:
//! it never adds a word the original does not say.
use super::*;
use crate::english::English;
use catalog_core::facts::contract::{InputBinding, Requirement};

/// A listing as its publisher describes it, and a faithful English rendering.
/// Statistics tables are reviewed contracts named by their table code, as
/// CBS's are named `statistics.cbs-*`: the id says it is a statistics table,
/// only its (foreign) description says of what. Other imports keep the
/// generated `operation.*` id.
struct Listing {
    id: u128,
    source: &'static str,
    name: &'static str,
    slug: &'static str,
    jurisdiction: Option<&'static str>,
    /// An identifier input that names the listing's register, if any.
    fact: Option<&'static str>,
    inputs: &'static [&'static str],
    text: &'static str,
    english: &'static str,
}

const LISTINGS: &[Listing] = &[
    // Statistics offices.
    Listing { id: 1, source: "cbs-statline", name: "CBS Statistics", slug: "statistics.cbs-80590ned", jurisdiction: Some("NL"), fact: None, inputs: &["periode", "geslacht"],
        text: "Arbeidsdeelname en werkloosheid per maand; seizoengecorrigeerd. Werkloze beroepsbevolking en werkloosheidspercentage naar geslacht en leeftijd.",
        english: "Labour participation and unemployment per month; seasonally adjusted. Unemployed labour force and unemployment rate by sex and age." },
    Listing { id: 2, source: "cbs-statline", name: "CBS Statistics", slug: "statistics.cbs-83131ned", jurisdiction: Some("NL"), fact: None, inputs: &["periode"],
        text: "Consumentenprijzen; prijsindex 2015=100. Jaarmutatie van de consumentenprijsindex (inflatie) per maand.",
        english: "Consumer prices; price index 2015=100. Annual change of the consumer price index (inflation) per month." },
    Listing { id: 3, source: "cbs-statline", name: "CBS Statistics", slug: "statistics.cbs-37230ned", jurisdiction: Some("NL"), fact: None, inputs: &["regio", "periode"],
        text: "Bevolkingsontwikkeling; regio per maand. Bevolking aan het begin van de periode, geboorte, sterfte en migratie per gemeente.",
        english: "Population development; region per month. Population at the start of the period, births, deaths and migration per municipality." },
    Listing { id: 4, source: "insee", name: "INSEE", slug: "statistics.insee-001688526", jurisdiction: Some("FR"), fact: None, inputs: &["periode", "region"],
        text: "Taux de chômage au sens du BIT par trimestre, en France et par région. Nombre de chômeurs et taux en pourcentage de la population active.",
        english: "ILO unemployment rate by quarter, in France and by region. Number of unemployed people and rate as a percentage of the labour force." },
    Listing { id: 5, source: "insee", name: "INSEE", slug: "statistics.insee-001759970", jurisdiction: Some("FR"), fact: None, inputs: &["periode"],
        text: "Indice des prix à la consommation (IPC) mensuel et glissement annuel : mesure de l'inflation en France.",
        english: "Monthly consumer price index (CPI) and year-on-year change: measure of inflation in France." },
    Listing { id: 6, source: "insee", name: "INSEE", slug: "statistics.insee-pop-legales", jurisdiction: Some("FR"), fact: None, inputs: &["commune", "annee"],
        text: "Populations légales des communes, départements et régions de France, publiées chaque année.",
        english: "Legal populations of the municipalities, departments and regions of France, published every year." },
    Listing { id: 7, source: "insee", name: "INSEE", slug: "statistics.insee-001656092", jurisdiction: Some("FR"), fact: None, inputs: &["periode", "secteur"],
        text: "Défaillances d'entreprises (faillites) par secteur d'activité et par mois : nombre de jugements d'ouverture de procédures collectives.",
        english: "Business failures (bankruptcies) by sector of activity and by month: number of judgments opening insolvency proceedings." },
    Listing { id: 8, source: "destatis", name: "Destatis GENESIS", slug: "statistics.destatis-13211-0002", jurisdiction: Some("DE"), fact: None, inputs: &["zeitraum", "bundesland"],
        text: "Arbeitslosenquote und Arbeitslose nach Bundesländern, Monatswerte (Bundesagentur für Arbeit).",
        english: "Unemployment rate and unemployed persons by federal state, monthly values (Federal Employment Agency)." },
    Listing { id: 9, source: "destatis", name: "Destatis GENESIS", slug: "statistics.destatis-61111-0002", jurisdiction: Some("DE"), fact: None, inputs: &["zeitraum"],
        text: "Verbraucherpreisindex für Deutschland: Veränderungsrate zum Vorjahresmonat (Inflationsrate), Monatswerte.",
        english: "Consumer price index for Germany: rate of change from the same month of the previous year (inflation rate), monthly values." },
    Listing { id: 10, source: "destatis", name: "Destatis GENESIS", slug: "statistics.destatis-12411-0010", jurisdiction: Some("DE"), fact: None, inputs: &["stichtag", "bundesland"],
        text: "Bevölkerungsstand: Bevölkerung nach Geschlecht, Nationalität und Bundesländern zum Stichtag.",
        english: "Population level: population by sex, nationality and federal state on the reference date." },
    Listing { id: 11, source: "destatis", name: "Destatis GENESIS", slug: "statistics.destatis-52411-0010", jurisdiction: Some("DE"), fact: None, inputs: &["zeitraum", "wirtschaftszweig"],
        text: "Insolvenzverfahren: beantragte Unternehmensinsolvenzen nach Wirtschaftszweigen, Monatswerte.",
        english: "Insolvency proceedings: applied company insolvencies by economic sector, monthly values." },
    Listing { id: 12, source: "ine-es", name: "INE España", slug: "statistics.ine-65334", jurisdiction: Some("ES"), fact: None, inputs: &["periodo", "comunidad"],
        text: "Tasa de paro por comunidad autónoma, sexo y trimestre (Encuesta de Población Activa, EPA).",
        english: "Unemployment rate by autonomous community, sex and quarter (Labour Force Survey, EPA)." },
    Listing { id: 13, source: "ine-es", name: "INE España", slug: "statistics.ine-50913", jurisdiction: Some("ES"), fact: None, inputs: &["periodo"],
        text: "Índice de Precios de Consumo (IPC): tasa de variación anual del índice general, mensual. Mide la inflación.",
        english: "Consumer Price Index (CPI): annual rate of change of the general index, monthly. Measures inflation." },
    Listing { id: 14, source: "ine-es", name: "INE España", slug: "statistics.ine-2852", jurisdiction: Some("ES"), fact: None, inputs: &["municipio"],
        text: "Población por municipios según el Padrón continuo, por sexo.",
        english: "Population by municipality according to the continuous municipal register (Padrón), by sex." },
    Listing { id: 15, source: "ine-es", name: "INE España", slug: "statistics.ine-3995", jurisdiction: Some("ES"), fact: None, inputs: &["periodo", "actividad"],
        text: "Estadística de Procedimiento Concursal: deudores concursados (quiebras) por sector de actividad económica, trimestral.",
        english: "Insolvency Proceedings Statistics: insolvent debtors (bankruptcies) by sector of economic activity, quarterly." },
    // Company registers and officers.
    Listing { id: 16, source: "handelsregister", name: "Unternehmensregister", slug: "operation.handelsregister.get-registerauszug", jurisdiction: Some("DE"), fact: None, inputs: &["firma", "registernummer"],
        text: "Handelsregisterauszug eines Unternehmens: Firma, Rechtsform, Sitz, Registergericht und Registernummer aus dem Unternehmensregister.",
        english: "Commercial register extract of a company: company name, legal form, registered office, register court and register number from the company register." },
    Listing { id: 17, source: "handelsregister", name: "Unternehmensregister", slug: "operation.handelsregister.get-vertretungsberechtigte", jurisdiction: Some("DE"), fact: None, inputs: &["firma", "registernummer"],
        text: "Vertretungsberechtigte Personen eines Unternehmens laut Handelsregister: Geschäftsführer, Vorstände und Prokuristen mit Bestellungsdatum.",
        english: "Persons authorised to represent a company according to the commercial register: managing directors, board members and authorised signatories with date of appointment." },
    Listing { id: 18, source: "registro-mercantil", name: "Registro Mercantil", slug: "operation.registro-mercantil.get-nota-simple", jurisdiction: Some("ES"), fact: None, inputs: &["denominacion", "cif"],
        text: "Nota simple del Registro Mercantil de una sociedad: denominación social, forma jurídica, domicilio social, CIF y datos registrales.",
        english: "Simple extract from the Commercial Register for a company: company name, legal form, registered office, tax ID (CIF) and registration details." },
    Listing { id: 19, source: "registro-mercantil", name: "Registro Mercantil", slug: "operation.registro-mercantil.get-administradores", jurisdiction: Some("ES"), fact: None, inputs: &["denominacion", "cif"],
        text: "Administradores y cargos vigentes de una sociedad inscritos en el Registro Mercantil: nombre, cargo y fecha de nombramiento.",
        english: "Current directors and officers of a company registered in the Commercial Register: name, position and date of appointment." },
    Listing { id: 20, source: "infogreffe", name: "Infogreffe", slug: "operation.infogreffe.get-extrait-kbis", jurisdiction: Some("FR"), fact: None, inputs: &["siren"],
        text: "Extrait Kbis d'une entreprise immatriculée au registre du commerce et des sociétés (RCS) : dénomination, forme juridique, siège social, numéro SIREN.",
        english: "Kbis extract of a company registered in the Trade and Companies Register (RCS): company name, legal form, registered office, SIREN number." },
    Listing { id: 21, source: "sirenic", name: "Sirenic", slug: "company.sirenic-entreprise-siren-dirigeants", jurisdiction: Some("FR"), fact: Some("company.registration.fr.siren"), inputs: &[],
        text: "Dirigeants d'une entreprise française par SIREN : nom, qualité et date de prise de fonction, issus du Registre national des entreprises (RNE).",
        english: "Directors of a French company by SIREN: name, role and start date, from the National Business Register (RNE)." },
    Listing { id: 22, source: "kvk-dutch-business-register", name: "KVK Business Register", slug: "operation.kvk-dutch-business-register.get-functionarissen", jurisdiction: Some("NL"), fact: Some("company_registry.kvknummer"), inputs: &[],
        text: "Functionarissen van een onderneming uit het Handelsregister: bestuurders, commissarissen en gevolmachtigden met functie en datum in functie.",
        english: "Officers of a business from the Trade Register: directors, supervisory board members and authorised representatives with position and start date." },
    // Tenders.
    Listing { id: 23, source: "boamp", name: "BOAMP", slug: "operation.boamp.get-avis", jurisdiction: Some("FR"), fact: None, inputs: &["mots_cles", "departement"],
        text: "Avis de marchés publics publiés au BOAMP : appels d'offres, avis d'attribution, acheteur public et date limite de réception des offres.",
        english: "Public procurement notices published in the BOAMP: calls for tenders, award notices, public buyer and deadline for receipt of tenders." },
    Listing { id: 24, source: "placsp", name: "Plataforma de Contratación del Sector Público", slug: "operation.placsp.get-licitaciones", jurisdiction: Some("ES"), fact: None, inputs: &["palabras_clave"],
        text: "Licitaciones y adjudicaciones de contratos del sector público en España: órgano de contratación, objeto del contrato y plazo de presentación de ofertas.",
        english: "Tenders and awards of public sector contracts in Spain: contracting authority, subject of the contract and deadline for submitting bids." },
    Listing { id: 25, source: "bund-ausschreibungen", name: "service.bund.de", slug: "operation.bund-ausschreibungen.get-ausschreibungen", jurisdiction: Some("DE"), fact: None, inputs: &["suchbegriff"],
        text: "Öffentliche Ausschreibungen und Vergabeverfahren des Bundes: Auftraggeber, Leistungsbeschreibung und Angebotsfrist.",
        english: "Public invitations to tender and award procedures of the federal government: contracting authority, description of services and deadline for bids." },
    // Addresses and geocoding.
    Listing { id: 26, source: "pdok", name: "PDOK Maps & Addresses", slug: "operation.pdok.get-locatieserver-free", jurisdiction: Some("NL"), fact: None, inputs: &["zoekterm"],
        text: "PDOK Locatieserver: zoek adressen, postcodes en woonplaatsen en krijg de coördinaten (RD en WGS84) en BAG-identificaties terug.",
        english: "PDOK Location Server: search addresses, postcodes and places and get back the coordinates (RD and WGS84) and BAG identifiers." },
    Listing { id: 27, source: "ban", name: "Base Adresse Nationale", slug: "operation.ban.get-search", jurisdiction: Some("FR"), fact: None, inputs: &["adresse"],
        text: "Géocodage d'une adresse en France avec la Base Adresse Nationale : coordonnées, code postal, commune et identifiant BAN.",
        english: "Geocoding of an address in France with the National Address Base: coordinates, postcode, municipality and BAN identifier." },
    Listing { id: 28, source: "cartociudad", name: "CartoCiudad", slug: "operation.cartociudad.get-geocoder", jurisdiction: Some("ES"), fact: None, inputs: &["direccion"],
        text: "Geocodificación de direcciones postales en España: coordenadas, código postal, municipio y provincia.",
        english: "Geocoding of postal addresses in Spain: coordinates, postcode, municipality and province." },
    Listing { id: 29, source: "bkg-geokodierung", name: "BKG Geokodierungsdienst", slug: "operation.bkg-geokodierung.get-adresse", jurisdiction: Some("DE"), fact: None, inputs: &["anschrift"],
        text: "Geokodierung von Adressen in Deutschland: Koordinaten, Postleitzahl und Gemeinde zu einer Anschrift.",
        english: "Geocoding of addresses in Germany: coordinates, postcode and municipality for an address." },
    // VAT numbers.
    Listing { id: 30, source: "bzst", name: "Bundeszentralamt für Steuern", slug: "operation.bzst.get-ust-idnr-bestaetigung", jurisdiction: Some("EU"), fact: None, inputs: &["ust_idnr"],
        text: "Bestätigung ausländischer Umsatzsteuer-Identifikationsnummern (USt-IdNr.): Gültigkeit einer USt-IdNr. eines anderen EU-Mitgliedstaats prüfen.",
        english: "Confirmation of foreign VAT identification numbers (USt-IdNr.): check the validity of a VAT number from another EU member state." },
    Listing { id: 31, source: "aeat", name: "Agencia Tributaria", slug: "operation.aeat.get-comprobacion-nif-iva", jurisdiction: Some("EU"), fact: None, inputs: &["nif_iva"],
        text: "Comprobación de un NIF-IVA intracomunitario: validez del número de IVA de un operador de la Unión Europea (VIES).",
        english: "Verification of an intra-Community VAT number: validity of the VAT number of an operator in the European Union (VIES)." },
    // News.
    Listing { id: 32, source: "nos", name: "NOS Nieuws", slug: "operation.nos.get-nieuws", jurisdiction: Some("NL"), fact: None, inputs: &["zoekterm"],
        text: "Het laatste nieuws van de NOS uit binnen- en buitenland: koppen, publicatietijd en link naar het artikel.",
        english: "The latest news from the NOS from home and abroad: headlines, publication time and link to the article." },
    Listing { id: 33, source: "lemonde", name: "Le Monde", slug: "operation.lemonde.get-actualites", jurisdiction: Some("FR"), fact: None, inputs: &["recherche"],
        text: "Dernières actualités du Monde : titres des articles, date de publication et lien.",
        english: "Latest news from Le Monde: article headlines, publication date and link." },
    Listing { id: 34, source: "tagesschau", name: "tagesschau.de", slug: "operation.tagesschau.get-nachrichten", jurisdiction: Some("DE"), fact: None, inputs: &["suchbegriff"],
        text: "Aktuelle Nachrichten der tagesschau: Schlagzeilen, Veröffentlichungszeit und Link zum Artikel.",
        english: "Current news from the tagesschau: headlines, publication time and link to the article." },
    Listing { id: 35, source: "elpais", name: "El País", slug: "operation.elpais.get-noticias", jurisdiction: Some("ES"), fact: None, inputs: &["busqueda"],
        text: "Últimas noticias de El País: titulares, fecha de publicación y enlace a la noticia.",
        english: "Latest news from El País: headlines, publication date and link to the story." },
];

/// A publisher's directory entry, in its own language.
struct Publisher {
    slug: &'static str,
    name: &'static str,
    name_english: Option<&'static str>,
    category: &'static str,
    text: &'static str,
    english: &'static str,
    tags: &'static [&'static str],
    tags_english: &'static str,
}

const PUBLISHERS: &[Publisher] = &[
    Publisher { slug: "insee", name: "INSEE", name_english: None, category: "statistics",
        text: "Institut national de la statistique et des études économiques : séries et indicateurs sur l'économie et la société françaises.",
        english: "National Institute of Statistics and Economic Studies: series and indicators on the French economy and society.",
        tags: &["chômage", "prix", "démographie"], tags_english: "unemployment, prices, demography" },
    Publisher { slug: "destatis", name: "Destatis GENESIS", name_english: None, category: "statistics",
        text: "Statistisches Bundesamt: amtliche Statistiken zu Wirtschaft, Arbeitsmarkt und Bevölkerung in Deutschland.",
        english: "Federal Statistical Office: official statistics on the economy, labour market and population in Germany.",
        tags: &["Arbeitsmarkt", "Preise", "Bevölkerung"], tags_english: "labour market, prices, population" },
    Publisher { slug: "ine-es", name: "INE España", name_english: Some("INE Spain"), category: "statistics",
        text: "Instituto Nacional de Estadística: estadísticas oficiales de economía, empleo y población de España.",
        english: "National Statistics Institute: official statistics on the economy, employment and population of Spain.",
        tags: &["empleo", "precios", "población"], tags_english: "employment, prices, population" },
    Publisher { slug: "handelsregister", name: "Unternehmensregister", name_english: Some("Company Register"), category: "company",
        text: "Registerauskünfte aus dem Handelsregister der deutschen Registergerichte.",
        english: "Register information from the commercial register of the German registry courts.",
        tags: &["Handelsregister", "Unternehmen"], tags_english: "commercial register, companies" },
    Publisher { slug: "registro-mercantil", name: "Registro Mercantil", name_english: Some("Commercial Register"), category: "company",
        text: "Información registral de sociedades mercantiles españolas.",
        english: "Registry information on Spanish commercial companies.",
        tags: &["sociedades", "registro"], tags_english: "companies, register" },
    Publisher { slug: "infogreffe", name: "Infogreffe", name_english: None, category: "company",
        text: "Informations légales sur les entreprises immatriculées au registre du commerce et des sociétés.",
        english: "Legal information on companies registered in the Trade and Companies Register.",
        tags: &["entreprises", "greffe"], tags_english: "companies, court registry" },
    Publisher { slug: "boamp", name: "BOAMP", name_english: None, category: "procurement",
        text: "Bulletin officiel des annonces des marchés publics.",
        english: "Official bulletin of public procurement notices.",
        tags: &["marchés publics", "appels d'offres"], tags_english: "public procurement, calls for tenders" },
    Publisher { slug: "placsp", name: "Plataforma de Contratación del Sector Público", name_english: Some("Public Sector Contracting Platform"), category: "procurement",
        text: "Perfiles de contratante y licitaciones del sector público español.",
        english: "Buyer profiles and tenders of the Spanish public sector.",
        tags: &["licitaciones", "contratación pública"], tags_english: "tenders, public procurement" },
    Publisher { slug: "bund-ausschreibungen", name: "service.bund.de", name_english: None, category: "procurement",
        text: "Ausschreibungen und Stellenangebote der Bundesverwaltung.",
        english: "Invitations to tender and job offers of the federal administration.",
        tags: &["Ausschreibungen", "Vergabe"], tags_english: "invitations to tender, contract award" },
    Publisher { slug: "ban", name: "Base Adresse Nationale", name_english: Some("National Address Base"), category: "location",
        text: "Référentiel national des adresses françaises et service de géocodage.",
        english: "National reference of French addresses and geocoding service.",
        tags: &["adresses", "géocodage"], tags_english: "addresses, geocoding" },
    Publisher { slug: "cartociudad", name: "CartoCiudad", name_english: None, category: "location",
        text: "Cartografía y direcciones postales de España.",
        english: "Cartography and postal addresses of Spain.",
        tags: &["direcciones", "geocodificación"], tags_english: "addresses, geocoding" },
    Publisher { slug: "bkg-geokodierung", name: "BKG Geokodierungsdienst", name_english: Some("BKG Geocoding Service"), category: "location",
        text: "Geokodierungsdienst des Bundesamts für Kartographie und Geodäsie.",
        english: "Geocoding service of the Federal Agency for Cartography and Geodesy.",
        tags: &["Adressen", "Geokodierung"], tags_english: "addresses, geocoding" },
    Publisher { slug: "bzst", name: "Bundeszentralamt für Steuern", name_english: Some("Federal Central Tax Office"), category: "finance",
        text: "Bestätigungsverfahren für Umsatzsteuer-Identifikationsnummern.",
        english: "Confirmation procedure for VAT identification numbers.",
        tags: &["Umsatzsteuer", "USt-IdNr."], tags_english: "VAT, VAT ID" },
    Publisher { slug: "aeat", name: "Agencia Tributaria", name_english: Some("Tax Agency"), category: "finance",
        text: "Servicios de la Agencia Estatal de Administración Tributaria.",
        english: "Services of the State Tax Administration Agency.",
        tags: &["IVA", "NIF"], tags_english: "VAT, tax ID" },
    Publisher { slug: "nos", name: "NOS Nieuws", name_english: Some("NOS News"), category: "news",
        text: "Nieuws van de Nederlandse Omroep Stichting.",
        english: "News from the Dutch Broadcasting Foundation.",
        tags: &["nieuws", "binnenland", "buitenland"], tags_english: "news, domestic, foreign" },
    Publisher { slug: "lemonde", name: "Le Monde", name_english: None, category: "news",
        text: "Quotidien d'information générale français.",
        english: "French general news daily.",
        tags: &["actualités", "presse"], tags_english: "news, press" },
    Publisher { slug: "tagesschau", name: "tagesschau.de", name_english: None, category: "news",
        text: "Nachrichten der ARD.",
        english: "News from the ARD.",
        tags: &["Nachrichten", "Inland", "Ausland"], tags_english: "news, domestic, foreign" },
    Publisher { slug: "elpais", name: "El País", name_english: None, category: "news",
        text: "Diario de información general español.",
        english: "Spanish general news daily.",
        tags: &["noticias", "prensa"], tags_english: "news, press" },
];

fn staged(template: &Service, listing: &Listing) -> Service {
    let mut service = template.clone();
    service.id = uuid::Uuid::from_u128(0x4d4c_0000 + listing.id);
    service.endpoint_id = uuid::Uuid::from_u128(0x4d4c_1000 + listing.id);
    service.unverified = false;
    service.api_slug = listing.source.into();
    service.name = listing.name.into();
    service.description = Some(listing.text.into());
    service.jurisdiction = listing.jurisdiction.map(str::to_owned);
    service.list_atomic = 10_000;
    service.contract.capability_slug = listing.slug.into();
    let facts: Vec<(String, String)> = listing
        .fact
        .map(|fact| (fact.to_owned(), "id".to_owned()))
        .into_iter()
        .chain(listing.inputs.iter().map(|input| {
            (
                format!("source_input.m{:04}.{input}", listing.id),
                (*input).to_owned(),
            )
        }))
        .collect();
    service.contract.requires = facts
        .iter()
        .map(|(fact, _)| Requirement {
            fact_type: fact.clone(),
            subject_role: "subject".into(),
            optional: false,
        })
        .collect();
    service.contract.input_map = facts
        .iter()
        .map(|(fact, name)| {
            let binding: InputBinding =
                serde_json::from_value(json!({"in":"query","name":name})).unwrap();
            (fact.clone(), binding)
        })
        .collect();
    let extract = template
        .contract
        .output_map
        .produces
        .values()
        .next()
        .cloned()
        .expect("template output");
    service.contract.output_map.produces = [(
        format!("source_document.m{:04}.response", listing.id),
        extract,
    )]
    .into_iter()
    .collect();
    service
}

/// The base calibration directory plus the listings above, and the
/// translator search reads requests with: with `translated`, the English
/// renderings a background pass leaves and the fake model; without, none.
fn directory(translated: bool) -> (&'static Catalog, &'static English) {
    static PLAIN: OnceLock<(Catalog, English)> = OnceLock::new();
    static TRANSLATED: OnceLock<(Catalog, std::sync::Arc<English>)> = OnceLock::new();
    let build = || {
        let base = plain();
        let template = base
            .services
            .iter()
            .find(|service| service.api_slug == "iban-check")
            .cloned()
            .expect("IBAN Check fixture");
        let mut services = base.services.clone();
        services.extend(LISTINGS.iter().map(|listing| staged(&template, listing)));
        let mut known_sources = base.known_sources.clone();
        for publisher in PUBLISHERS {
            known_sources.push(serde_json::from_value(json!({
                "slug":publisher.slug,"name":publisher.name,"block_reason":null,
                "metadata":{"description":publisher.text,"category":publisher.category,"tags":publisher.tags,
                    "capability_descriptions":LISTINGS.iter().filter(|listing| listing.source == publisher.slug)
                        .map(|listing| (listing.slug.to_owned(), json!(listing.text))).collect::<serde_json::Map<_, _>>()}
            })).unwrap());
        }
        let version = crate::state::digest(&(&services, &known_sources));
        Catalog {
            services,
            vocabulary: Default::default(),
            version: format!("multilingual-{version}"),
            rejected: vec![],
            by_capability: Default::default(),
            by_output: Default::default(),
            known_sources,
            english: Default::default(),
        }
    };
    if !translated {
        let (catalog, english) = PLAIN.get_or_init(|| (build(), English::disabled()));
        return (catalog, english);
    }
    let (catalog, english) = TRANSLATED.get_or_init(|| {
        let mut catalog = build();
        let dictionary = dictionary(&catalog);
        let borrowed: Vec<(&str, &str)> = dictionary.iter().map(|(text, english)| (text.as_str(), english.as_str())).collect();
        let (english, plan, report) = translate_catalog(&mut catalog, &borrowed);
        println!(
            "multilingual directory: {} texts, {} clearly English, {} sent in {} calls, {} translated, {} without a rendering (brand names)",
            plan.units, plan.english, plan.pending.len(), report.calls, report.translated, report.failed
        );
        (catalog, english)
    });
    (catalog, english.as_ref())
}

/// Every rendering the fake translator knows for the directory.
fn dictionary(catalog: &Catalog) -> Vec<(String, String)> {
    let mut entries = base_dictionary(catalog);
    for listing in LISTINGS {
        entries.push((listing.text.into(), listing.english.into()));
    }
    for (text, english) in QUERY_DICTIONARY.iter().chain(PARSES_IN_ENGLISH) {
        entries.push(((*text).to_owned(), (*english).to_owned()));
    }
    for publisher in PUBLISHERS {
        entries.push((publisher.text.into(), publisher.english.into()));
        let tags: Vec<String> = publisher.tags.iter().map(|tag| (*tag).to_owned()).collect();
        entries.push((tags_text(&tags), publisher.tags_english.into()));
        if let Some(english) = publisher.name_english {
            entries.push((publisher.name.into(), english.into()));
        }
    }
    entries
}

/// One question, its parse and the candidates that answer it.
struct Case {
    question: &'static str,
    ask: Ask<'static>,
    /// `source:capability` prefixes of the candidates that answer it.
    relevant: &'static [&'static str],
}

fn company(label: &'static str) -> Option<(&'static str, &'static str)> {
    Some((label, "company"))
}
fn country(label: &'static str) -> Option<(&'static str, &'static str)> {
    Some((label, "country"))
}
fn address(label: &'static str) -> Option<(&'static str, &'static str)> {
    Some((label, "address"))
}

const UK_REGISTER: &[&str] = &["companies-house:company.registry-profile"];
const NL_REGISTER: &[&str] = &[
    "kvk-dutch-business-register:company.registry-profile",
    "kvk-dutch-business-register:company_registry.lookup",
];
const FR_REGISTER: &[&str] = &[
    "infogreffe:",
    "sirenic:company.registry-profile",
    "sirenic:company.fr-registry-search",
];
const DE_REGISTER: &[&str] = &[
    "handelsregister:operation.handelsregister.get-registerauszug",
    "sirenic:company.registry-profile",
    "sirenic:company.eu-registry-search",
];
const ES_REGISTER: &[&str] = &[
    "registro-mercantil:operation.registro-mercantil.get-nota-simple",
    "sirenic:company.registry-profile",
    "sirenic:company.eu-registry-search",
];
const VAT: &[&str] = &[
    "vies:vat.eu.",
    "sirenic:company.sirenic-tva-verifier-numero",
    "bzst:",
    "aeat:",
];
const NEWS: &[&str] = &[
    "reuters:news.",
    "bloomberg:news.",
    "nos:",
    "lemonde:",
    "tagesschau:",
    "elpais:",
];

fn registry(
    language: &'static str,
    label: &'static str,
    hint: Option<&'static str>,
) -> Ask<'static> {
    Ask {
        slug: "company.registry.extract",
        name: "Company register extract",
        description: "Retrieves the official register record of a company, such as its legal name, legal form, registered office and registration number.",
        inputs: &["company_name"],
        outputs: &["legal_name", "legal_form", "registered_office", "registration_number"],
        subject: company(label),
        hint,
        language,
        ..Default::default()
    }
}
fn officers(language: &'static str, label: &'static str) -> Ask<'static> {
    Ask {
        slug: "company.officers",
        name: "Company directors",
        description: "Retrieves the current directors and officers of a company with their roles and appointment dates.",
        inputs: &["company_name"],
        outputs: &["director_name", "role", "appointment_date"],
        subject: company(label),
        language,
        ..Default::default()
    }
}
fn unemployment(language: &'static str, label: &'static str) -> Ask<'static> {
    Ask {
        slug: "research.statistics.unemployment",
        name: "Unemployment rate",
        description: "Retrieves the official unemployment rate of a country over time.",
        inputs: &["country"],
        outputs: &["unemployment_rate", "period"],
        subject: country(label),
        language,
        ..Default::default()
    }
}
fn inflation(language: &'static str, label: &'static str) -> Ask<'static> {
    Ask {
        slug: "research.statistics.inflation",
        name: "Inflation rate",
        description:
            "Retrieves the annual inflation rate of a country based on its consumer price index.",
        inputs: &["country"],
        outputs: &["inflation_rate", "consumer_price_index", "period"],
        subject: country(label),
        language,
        ..Default::default()
    }
}
fn population(language: &'static str, label: &'static str) -> Ask<'static> {
    Ask {
        slug: "research.statistics.population",
        name: "Population statistics",
        description:
            "Retrieves the official population of a country and its regions or municipalities.",
        inputs: &["country"],
        outputs: &["population", "region", "period"],
        subject: country(label),
        language,
        ..Default::default()
    }
}
fn tenders(language: &'static str, label: &'static str) -> Ask<'static> {
    Ask {
        slug: "procurement.tenders.search",
        name: "Public tenders",
        description: "Searches public procurement tender notices published by government buyers.",
        inputs: &["country"],
        outputs: &["notice_title", "buyer", "deadline"],
        subject: country(label),
        language,
        ..Default::default()
    }
}
fn geocode(
    language: &'static str,
    label: &'static str,
    identifiers: &'static [(&'static str, &'static str)],
) -> Ask<'static> {
    Ask {
        slug: "location.address.coordinates",
        name: "Address coordinates",
        description: "Geocodes a postal address and returns its coordinates.",
        inputs: &["address"],
        outputs: &["latitude", "longitude", "postcode", "municipality"],
        subject: address(label),
        identifiers,
        language,
        ..Default::default()
    }
}
fn vat(
    language: &'static str,
    identifiers: &'static [(&'static str, &'static str)],
) -> Ask<'static> {
    Ask {
        slug: "finance.vat.validate",
        name: "VAT number validation",
        description: "Checks whether a VAT number is valid and registered.",
        inputs: &["vat_number"],
        outputs: &["valid", "registered_name"],
        // "If only a number is given, that number."
        subject: Some((identifiers[0].1, "company")),
        identifiers,
        language,
        ..Default::default()
    }
}
fn news(language: &'static str, label: &'static str) -> Ask<'static> {
    Ask {
        slug: "research.news.company",
        name: "Latest company news",
        description: "Retrieves recent news articles about a company.",
        inputs: &["company_name"],
        outputs: &["headlines", "published_at", "article_url"],
        subject: company(label),
        language,
        ..Default::default()
    }
}

fn cases() -> Vec<Case> {
    vec![
        // Company register lookup.
        Case { question: "Show the Companies House record of Tesco PLC.", ask: registry("en", "Tesco PLC", None), relevant: UK_REGISTER },
        Case { question: "Geef de KVK-gegevens van Coolblue B.V.", ask: registry("nl", "Coolblue B.V.", Some("kvk")), relevant: NL_REGISTER },
        Case { question: "Extrait Kbis de Exemple SAS", ask: registry("fr", "Exemple SAS", None), relevant: FR_REGISTER },
        Case { question: "Handelsregisterauszug der Muster GmbH", ask: registry("de", "Muster GmbH", None), relevant: DE_REGISTER },
        Case { question: "Datos registrales de Ejemplo S.L.", ask: registry("es", "Ejemplo S.L.", None), relevant: ES_REGISTER },
        // Company officers.
        Case { question: "Who are the directors of Tesco PLC?", ask: officers("en", "Tesco PLC"),
            relevant: &["companies-house:company.officers", "sirenic:company.sirenic-eu-entreprise-gb-company-number-dirigeants"] },
        Case { question: "Wie zijn de bestuurders van Coolblue B.V.?", ask: officers("nl", "Coolblue B.V."),
            relevant: &["kvk-dutch-business-register:operation.kvk-dutch-business-register.get-functionarissen"] },
        Case { question: "Qui sont les dirigeants de Exemple SAS ?", ask: officers("fr", "Exemple SAS"),
            relevant: &["sirenic:company.sirenic-entreprise-siren-dirigeants"] },
        Case { question: "Wer sind die Geschäftsführer der Muster GmbH?", ask: officers("de", "Muster GmbH"),
            relevant: &["handelsregister:operation.handelsregister.get-vertretungsberechtigte"] },
        Case { question: "¿Quiénes son los administradores de Ejemplo S.L.?", ask: officers("es", "Ejemplo S.L."),
            relevant: &["registro-mercantil:operation.registro-mercantil.get-administradores"] },
        // Unemployment.
        Case { question: "What is the unemployment rate in Germany?", ask: unemployment("en", "Germany"), relevant: &["destatis:statistics.destatis-13211-0002"] },
        Case { question: "Hoe hoog is de werkloosheid in Nederland?", ask: unemployment("nl", "Nederland"), relevant: &["cbs-statline:statistics.cbs-80590ned"] },
        Case { question: "Quel est le taux de chômage en France ?", ask: unemployment("fr", "France"), relevant: &["insee:statistics.insee-001688526"] },
        Case { question: "Wie hoch ist die Arbeitslosenquote in Deutschland?", ask: unemployment("de", "Deutschland"), relevant: &["destatis:statistics.destatis-13211-0002"] },
        Case { question: "¿Cuál es la tasa de paro en España?", ask: unemployment("es", "España"), relevant: &["ine-es:statistics.ine-65334"] },
        // Inflation.
        Case { question: "What is the inflation rate in Spain?", ask: inflation("en", "Spain"), relevant: &["ine-es:statistics.ine-50913"] },
        Case { question: "Wat is de inflatie in Nederland?", ask: inflation("nl", "Nederland"), relevant: &["cbs-statline:statistics.cbs-83131ned"] },
        Case { question: "Quelle est l'inflation en France ?", ask: inflation("fr", "France"), relevant: &["insee:statistics.insee-001759970"] },
        Case { question: "Wie hoch ist die Inflationsrate in Deutschland?", ask: inflation("de", "Deutschland"), relevant: &["destatis:statistics.destatis-61111-0002"] },
        Case { question: "¿Cuál es la inflación en España?", ask: inflation("es", "España"), relevant: &["ine-es:statistics.ine-50913"] },
        // Population.
        Case { question: "Population of France by municipality", ask: population("en", "France"), relevant: &["insee:statistics.insee-pop-legales"] },
        Case { question: "Hoeveel inwoners heeft Nederland?", ask: population("nl", "Nederland"), relevant: &["cbs-statline:statistics.cbs-37230ned"] },
        Case { question: "Quelle est la population de la France ?", ask: population("fr", "France"), relevant: &["insee:statistics.insee-pop-legales"] },
        Case { question: "Wie viele Einwohner hat Deutschland?", ask: population("de", "Deutschland"), relevant: &["destatis:statistics.destatis-12411-0010"] },
        Case { question: "¿Cuál es la población de España?", ask: population("es", "España"), relevant: &["ine-es:statistics.ine-2852"] },
        // Bankruptcies.
        Case { question: "Company bankruptcies in Spain by sector", ask: Ask {
                slug: "research.bankruptcies.bySector", name: "Bankruptcies by sector",
                description: "Retrieves the number of company bankruptcies per economic sector in a country.",
                inputs: &["country"], outputs: &["sector", "bankruptcy_count", "period"], subject: country("Spain"), ..Default::default() },
            relevant: &["ine-es:statistics.ine-3995"] },
        Case { question: "Faillissementen per sector in Nederland", ask: Ask {
                slug: "research.bankruptcies.bySector", name: "Bankruptcies by sector",
                description: "Retrieves the number of company bankruptcies per economic sector in a country.",
                inputs: &["country"], outputs: &["sector", "bankruptcy_count", "period"], subject: country("Nederland"), language: "nl", ..Default::default() },
            relevant: &["cbs-statline:statistics.cbs-bankrupt"] },
        Case { question: "Défaillances d'entreprises en France par secteur", ask: Ask {
                slug: "research.business.failures", name: "Business failures by sector",
                description: "Retrieves the number of business failures per economic sector in a country.",
                inputs: &["country"], outputs: &["sector", "business_failure_count", "period"], subject: country("France"), language: "fr", ..Default::default() },
            relevant: &["insee:statistics.insee-001656092"] },
        Case { question: "Unternehmensinsolvenzen in Deutschland nach Wirtschaftszweigen", ask: Ask {
                slug: "research.insolvencies.bySector", name: "Company insolvencies by sector",
                description: "Retrieves the number of company insolvencies per economic sector in a country.",
                inputs: &["country"], outputs: &["sector", "insolvency_count", "period"], subject: country("Deutschland"), language: "de", ..Default::default() },
            relevant: &["destatis:statistics.destatis-52411-0010"] },
        Case { question: "Quiebras de empresas en España por sector", ask: Ask {
                slug: "research.bankruptcies.bySector", name: "Company bankruptcies by sector",
                description: "Retrieves the number of company bankruptcies per economic sector in a country.",
                inputs: &["country"], outputs: &["sector", "bankruptcy_count", "period"], subject: country("España"), language: "es", ..Default::default() },
            relevant: &["ine-es:statistics.ine-3995"] },
        // Tenders.
        Case { question: "Public tenders in the Netherlands", ask: tenders("en", "Netherlands"),
            relevant: &["tenderned:", "ted:"] },
        Case { question: "Openbare aanbestedingen in Nederland", ask: tenders("nl", "Nederland"),
            relevant: &["tenderned:", "ted:"] },
        Case { question: "Appels d'offres publics en France", ask: tenders("fr", "France"),
            relevant: &["boamp:", "sirenic:company.sirenic-entreprise-siren-marches-publics"] },
        Case { question: "Öffentliche Ausschreibungen in Deutschland", ask: tenders("de", "Deutschland"),
            relevant: &["bund-ausschreibungen:"] },
        Case { question: "Licitaciones públicas en España", ask: tenders("es", "España"),
            relevant: &["placsp:"] },
        // Addresses.
        Case { question: "Coordinates of Damrak 1, 1012 LG Amsterdam", ask: geocode("en", "Damrak 1, 1012 LG Amsterdam", &[("postcode", "1012 LG")]), relevant: &["pdok:"] },
        Case { question: "Wat zijn de coördinaten van Damrak 1, 1012 LG Amsterdam?", ask: geocode("nl", "Damrak 1, 1012 LG Amsterdam", &[("postcode", "1012 LG")]), relevant: &["pdok:"] },
        Case { question: "Coordonnées GPS du 1 rue de Rivoli, 75001 Paris, France", ask: geocode("fr", "1 rue de Rivoli, 75001 Paris, France", &[]), relevant: &["ban:"] },
        Case { question: "Koordinaten von Unter den Linden 1, 10117 Berlin, Deutschland", ask: geocode("de", "Unter den Linden 1, 10117 Berlin, Deutschland", &[]), relevant: &["bkg-geokodierung:"] },
        Case { question: "Coordenadas de la Calle de Alcalá 1, 28014 Madrid, España", ask: geocode("es", "Calle de Alcalá 1, 28014 Madrid, España", &[]), relevant: &["cartociudad:"] },
        // VAT numbers.
        Case { question: "Is VAT number NL810433941B01 valid?", ask: vat("en", &[("vat_number", "NL810433941B01")]), relevant: VAT },
        Case { question: "Is btw-nummer NL810433941B01 geldig?", ask: vat("nl", &[("vat_number", "NL810433941B01")]), relevant: VAT },
        Case { question: "Le numéro de TVA FR40303265045 est-il valide ?", ask: vat("fr", &[("vat_number", "FR40303265045")]), relevant: VAT },
        Case { question: "Ist die USt-IdNr. FR40303265045 gültig?", ask: vat("de", &[("vat_number", "FR40303265045")]), relevant: VAT },
        Case { question: "¿Es válido el NIF-IVA ESA28015865?", ask: vat("es", &[("vat_number", "ESA28015865")]), relevant: VAT },
        // News.
        Case { question: "Latest news about Shell", ask: news("en", "Shell"), relevant: NEWS },
        Case { question: "Laatste nieuws over Shell", ask: news("nl", "Shell"), relevant: NEWS },
        Case { question: "Dernières nouvelles sur TotalEnergies", ask: news("fr", "TotalEnergies"), relevant: NEWS },
        Case { question: "Neueste Nachrichten über Siemens", ask: news("de", "Siemens"), relevant: NEWS },
        Case { question: "Últimas noticias sobre Telefónica", ask: news("es", "Telefónica"), relevant: NEWS },
    ]
}

fn relevant(case: &Case, candidate: &str) -> bool {
    case.relevant
        .iter()
        .any(|prefix| candidate.starts_with(prefix))
}

/// Recall at 1 and at 3 over `cases`, and the cases missed at 3.
fn recall(cases: &[Case], catalog: &Catalog, english: &English) -> (usize, usize, Vec<String>) {
    let (mut first, mut top) = (0, 0);
    let mut missed = Vec::new();
    for case in cases {
        let found = offered_by(&case.ask, catalog, english);
        if found
            .first()
            .is_some_and(|candidate| relevant(case, candidate))
        {
            first += 1;
        }
        if found
            .iter()
            .take(3)
            .any(|candidate| relevant(case, candidate))
        {
            top += 1;
        } else {
            missed.push(format!("{} -> {found:?}", case.question));
        }
    }
    (first, top, missed)
}

#[test]
fn every_language_and_domain_offers_a_relevant_source_first() {
    let cases = cases();
    let total = cases.len();
    assert!(total >= 45);
    let (catalog, english) = directory(true);
    for case in &cases {
        let found = offered_by(&case.ask, catalog, english);
        assert!(
            found
                .first()
                .is_some_and(|candidate| relevant(case, candidate)),
            "{} ({}): {found:?}",
            case.question,
            case.ask.slug
        );
    }
    let (plain, off) = directory(false);
    let (before_first, before_top, missed) = recall(&cases, plain, off);
    let (after_first, after_top, _) = recall(&cases, catalog, english);
    println!("multilingual calibration, {total} questions: without English renderings recall@1 {before_first}/{total}, recall@3 {before_top}/{total}; with them recall@1 {after_first}/{total}, recall@3 {after_top}/{total}");
    for miss in missed {
        println!("  missed without English renderings: {miss}");
    }
    assert_eq!((after_first, after_top), (total, total));
}

/// Parses as the model wrote them before the prompt asked for English
/// whatever the question's language, and as a connected chatbot may still
/// send them: in the question's language.
const PARSES_IN_ENGLISH: &[(&str, &str)] = &[
    ("Werkloosheidspercentage", "Unemployment rate"),
    ("Geeft het officiële werkloosheidspercentage van een land.", "Returns the official unemployment rate of a country."),
    ("land", "country"),
    ("werkloosheidspercentage, periode", "unemployment rate, period"),
    ("Taux d'inflation", "Inflation rate"),
    ("Donne le taux d'inflation annuel d'un pays à partir de l'indice des prix à la consommation.", "Gives the annual inflation rate of a country based on the consumer price index."),
    ("pays", "country"),
    ("taux d inflation, periode", "inflation rate, period"),
    ("Geschäftsführer eines Unternehmens", "Managing directors of a company"),
    ("Liefert die aktuellen Geschäftsführer und Vorstände eines Unternehmens mit Bestellungsdatum.", "Returns the current managing directors and board members of a company with their appointment date."),
    ("firmenname", "company name"),
    ("name, funktion, bestellungsdatum", "name, role, appointment date"),
    ("Licitaciones públicas", "Public tenders"),
    ("Busca anuncios de licitaciones de contratos públicos.", "Searches notices of public contract tenders."),
    ("pais", "country"),
    ("titulo, organo de contratacion, plazo", "title, contracting authority, deadline"),
    ("Laatste nieuws over een bedrijf", "Latest news about a company"),
    ("Haalt recente nieuwsartikelen over een bedrijf op.", "Retrieves recent news articles about a company."),
    ("bedrijfsnaam", "company name"),
    ("koppen, publicatiedatum", "headlines, publication date"),
];

/// Each about another country than the question's language: the parse's
/// wording meets the listing's only in English.
fn parses_in_their_language() -> Vec<Case> {
    vec![
        Case { question: "Hoe hoog is de werkloosheid in Duitsland?", ask: Ask {
                slug: "research.statistics.unemployment", name: "Werkloosheidspercentage",
                description: "Geeft het officiële werkloosheidspercentage van een land.",
                inputs: &["land"], outputs: &["werkloosheidspercentage", "periode"], subject: country("Duitsland"), language: "nl", ..Default::default() },
            relevant: &["destatis:statistics.destatis-13211-0002"] },
        Case { question: "Quel est le taux d'inflation en Espagne ?", ask: Ask {
                slug: "research.statistics.inflation", name: "Taux d'inflation",
                description: "Donne le taux d'inflation annuel d'un pays à partir de l'indice des prix à la consommation.",
                inputs: &["pays"], outputs: &["taux_d_inflation", "periode"], subject: country("Espagne"), language: "fr", ..Default::default() },
            relevant: &["ine-es:statistics.ine-50913"] },
        Case { question: "Wer sind die Geschäftsführer der Exemple SAS?", ask: Ask {
                slug: "company.officers", name: "Geschäftsführer eines Unternehmens",
                description: "Liefert die aktuellen Geschäftsführer und Vorstände eines Unternehmens mit Bestellungsdatum.",
                inputs: &["firmenname"], outputs: &["name", "funktion", "bestellungsdatum"], subject: company("Exemple SAS"), language: "de", ..Default::default() },
            relevant: &["sirenic:company.sirenic-entreprise-siren-dirigeants"] },
        Case { question: "Licitaciones públicas en Alemania", ask: Ask {
                slug: "procurement.tenders.search", name: "Licitaciones públicas",
                description: "Busca anuncios de licitaciones de contratos públicos.",
                inputs: &["pais"], outputs: &["titulo", "organo_de_contratacion", "plazo"], subject: country("Alemania"), language: "es", ..Default::default() },
            relevant: &["bund-ausschreibungen:"] },
        Case { question: "Laatste nieuws over Shell", ask: Ask {
                slug: "research.news.company", name: "Laatste nieuws over een bedrijf",
                description: "Haalt recente nieuwsartikelen over een bedrijf op.",
                inputs: &["bedrijfsnaam"], outputs: &["koppen", "publicatiedatum"], subject: company("Shell"), language: "nl", ..Default::default() },
            relevant: NEWS },
    ]
}

#[test]
fn a_parse_left_in_the_questions_language_is_also_read_in_english() {
    let cases = parses_in_their_language();
    let (catalog, english) = directory(true);
    // Without reading the request in English (translation off, failed or
    // late), search takes the parse as it is.
    let off = English::disabled();
    let (untranslated_first, untranslated_top, _) = recall(&cases, catalog, &off);
    let (first, top, missed) = recall(&cases, catalog, english);
    println!("parses in the question's language, {} questions: as they are recall@1 {untranslated_first}, recall@3 {untranslated_top}; read in English recall@1 {first}, recall@3 {top}", cases.len());
    assert_eq!((first, top), (cases.len(), cases.len()), "{missed:?}");
}

#[test]
fn english_renderings_only_add_candidates_to_what_the_original_text_finds() {
    // Every candidate the original wording makes relevant stays relevant:
    // translation indexes an English rendering next to it, never instead.
    let (plain, _) = directory(false);
    let (translated, _) = directory(true);
    let (plain_index, translated_index) = (index(plain), index(translated));
    for case in cases().iter().chain(&parses_in_their_language()) {
        let parsed = case.ask.parsed();
        let relevant = |catalog: &Catalog| -> BTreeSet<String> {
            let index = if std::ptr::eq(catalog, plain) {
                &plain_index
            } else {
                &translated_index
            };
            let capability = &parsed.capabilities[0];
            let wanted = wanted(capability, &parsed);
            let floor = relevance::Request::new(capability, &parsed);
            index
                .documents
                .iter()
                .filter(|candidate| {
                    score(&wanted, candidate, index) > 0.0
                        && floor.passes(&wanted, candidate, index, false)
                })
                .map(|candidate| match candidate.target {
                    Target::Endpoint(position) => {
                        catalog.services[position].endpoint_id.to_string()
                    }
                    Target::Source(position) => catalog.known_sources[position].slug.clone(),
                })
                .collect()
        };
        let (before, after) = (relevant(plain), relevant(translated));
        assert!(
            before.is_subset(&after),
            "{}: lost {:?}",
            case.question,
            before.difference(&after).collect::<Vec<_>>()
        );
    }
}

#[test]
fn the_earlier_calibration_answers_the_same_with_or_without_english_renderings() {
    // The base directory's own renderings (the WebberSites keywords, CBS's
    // Dutch tags) change none of the earlier questions' answers.
    let off = English::disabled();
    for ask in [
        profile("Coolblue"),
        profile("Coolblue B.V."),
        profile("Acme Ltd"),
        Ask {
            slug: "location.time.current",
            name: "Current local time",
            description: "Retrieve the current local time and timezone for a city.",
            inputs: &["city"],
            outputs: &["local_time", "timezone"],
            subject: Some(("Amsterdam", "city")),
            ..Default::default()
        },
        Ask {
            slug: "research.bankruptcies.bySector",
            name: "Bankruptcies by sector",
            description: "Compares the number of bankruptcies per sector in a country.",
            inputs: &["country"],
            outputs: &["sector", "bankruptcy_count"],
            subject: Some(("Netherlands", "country")),
            ..Default::default()
        },
        Ask {
            slug: "finance.vat.validate",
            name: "VAT number validation",
            description: "Check whether a VAT number is valid and registered.",
            inputs: &["vat_number"],
            outputs: &["valid", "registered_name"],
            subject: Some(("Coolblue", "company")),
            identifiers: &[("vat_number", "NL810433941B01")],
            ..Default::default()
        },
    ] {
        assert_eq!(
            offered_by(&ask, plain(), &off),
            offered(&ask),
            "{} / {:?}",
            ask.slug,
            ask.subject
        );
    }
    let catalog = catalog();
    let plan_units = catalog.english.len();
    assert!(plan_units >= 3, "{plan_units}");
}
