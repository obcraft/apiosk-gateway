//! Map (#50): source coordinates only, in a CRS the source states or that the
//! coordinates prove. No geocoding; a feature without a location stays in the list.
use super::resolve::{ensure, fail, valid_relative, Checked, Issue, Rows, Scope};
use crate::answer_blocks::{
    BlockIssue, Coverage, Crs, GeometryFormat, GeometryRef, MapBlock, MapFeature, MAX_MAP_COORDINATES, MAX_MAP_FEATURES,
};
use serde_json::{json, Value};

/// Source fields whose name states the CRS (PDOK Locatieserver, BAG).
const KNOWN_FIELDS: [(&str, Crs); 6] = [
    ("centroide_ll", Crs::Epsg4326),
    ("centroid_ll", Crs::Epsg4326),
    ("centroid_wgs84", Crs::Epsg4326),
    ("geometrie_ll", Crs::Epsg4326),
    ("centroide_rd", Crs::Epsg28992),
    ("centroid_rd", Crs::Epsg28992),
];

/// EPSG code from a stated CRS: "EPSG:28992", "urn:ogc:def:crs:EPSG::4326",
/// an OGC URL, "CRS84" or a bare code.
fn stated_crs(value: &Value) -> Option<Crs> {
    let text = match value {
        Value::Number(number) => number.to_string(),
        Value::String(text) => text.trim().to_owned(),
        Value::Object(_) => return stated_crs(&value["properties"]["name"]),
        _ => return None,
    };
    if text.to_ascii_uppercase().ends_with("CRS84") {
        return Some(Crs::Epsg4326);
    }
    let code = text.rsplit([':', '/']).next().filter(|code| !code.is_empty() && code.bytes().all(|b| b.is_ascii_digit()))?;
    match code {
        "4326" => Some(Crs::Epsg4326),
        "28992" => Some(Crs::Epsg28992),
        "3857" => Some(Crs::Epsg3857),
        _ => None,
    }
}

fn position(value: &Value) -> Option<(f64, f64)> {
    let items = value.as_array().filter(|items| (2..=3).contains(&items.len()))?;
    Some((items[0].as_f64()?, items[1].as_f64()?))
}

/// Every position of a GeoJSON coordinates array at the depth its type needs.
fn positions(coordinates: &Value, depth: usize, out: &mut Vec<(f64, f64)>) -> Option<()> {
    if depth == 0 {
        out.push(position(coordinates)?);
        return Some(());
    }
    let items = coordinates.as_array().filter(|items| !items.is_empty())?;
    items.iter().try_for_each(|item| positions(item, depth - 1, out))
}

fn wkt_point(text: &str) -> Option<(f64, f64)> {
    let inner = text.trim().strip_prefix("POINT")?.trim_start().strip_prefix('(')?.strip_suffix(')')?;
    let mut values = inner.split_whitespace().map(str::parse::<f64>);
    let point = (values.next()?.ok()?, values.next()?.ok()?);
    values.next().is_none().then_some(point)
}

fn geographic((lon, lat): (f64, f64)) -> bool {
    (-180.0..=180.0).contains(&lon) && (-90.0..=90.0).contains(&lat)
}

/// Copy a geometry from `root` (a document, or one record of a collection)
/// with its CRS. None when the source states no location.
fn locate(root: &Value, template: &GeometryRef, path: &str) -> Checked<Option<(Value, Crs, usize)>> {
    let Some(target) = root.pointer(&template.pointer).filter(|target| !target.is_null()) else { return Ok(None) };
    let invalid = || Issue { code: BlockIssue::InvalidGeometry, path: path.to_owned() };
    let (geometry, points, stated) = match template.format {
        GeometryFormat::Geojson => {
            let kind = target["type"].as_str().ok_or_else(invalid)?;
            let depth = match kind {
                "Point" => 0,
                "MultiPoint" | "LineString" => 1,
                "Polygon" => 2,
                "MultiPolygon" => 3,
                _ => return fail(BlockIssue::InvalidGeometry, path),
            };
            let mut points = Vec::new();
            positions(&target["coordinates"], depth, &mut points).ok_or_else(invalid)?;
            (json!({"type": kind, "coordinates": target["coordinates"]}), points, target.get("crs").and_then(stated_crs))
        }
        GeometryFormat::WktPoint => {
            let (x, y) = target.as_str().and_then(wkt_point).ok_or_else(invalid)?;
            (json!({"type": "Point", "coordinates": [x, y]}), vec![(x, y)], None)
        }
        GeometryFormat::LatLon => {
            let lon_pointer = template.lon_pointer.as_deref().ok_or_else(invalid)?;
            let lat = target.as_f64().ok_or_else(invalid)?;
            let lon = root.pointer(lon_pointer).and_then(Value::as_f64).ok_or_else(invalid)?;
            ensure(geographic((lon, lat)), BlockIssue::InvalidGeometry, path)?;
            return Ok(Some((json!({"type": "Point", "coordinates": [lon, lat]}), Crs::Epsg4326, 1)));
        }
    };
    let field = template.pointer.rsplit('/').next().unwrap_or_default();
    let crs = match template.crs_pointer.as_deref() {
        Some(pointer) => root.pointer(pointer).and_then(stated_crs),
        None => KNOWN_FIELDS.iter().find(|(name, _)| *name == field).map(|(_, crs)| *crs).or(stated),
    }
    .or_else(|| (template.crs_pointer.is_none() && points.iter().all(|point| geographic(*point))).then_some(Crs::Epsg4326));
    let crs = crs.ok_or_else(|| Issue { code: BlockIssue::UnsupportedCrs, path: path.to_owned() })?;
    Ok(Some((geometry, crs, points.len())))
}

fn geometry_ref(template: &GeometryRef, located: Option<(Value, Crs, usize)>) -> Option<(GeometryRef, Crs, usize)> {
    located.map(|(geometry, crs, count)| (GeometryRef { geometry, crs: Some(crs), ..template.clone() }, crs, count))
}

pub(super) fn map(block: MapBlock, scope: &mut Scope) -> Checked<(MapBlock, Option<Coverage>)> {
    if let Some(asset) = &block.asset {
        let document = scope.document(&asset.result_ref, "asset/result_ref")?;
        let captured = document["report_assets"]["assets"].get(asset.asset_index).is_some_and(|a| a["kind"] == "map");
        ensure(captured, BlockIssue::UnknownReference, "asset/asset_index")?;
    }
    let mut located: Vec<(MapFeature, Option<(Crs, usize)>)> = Vec::new();
    let coverage = match &block.collection {
        Some(collection) => {
            let rows = scope.collection(collection, "collection")?;
            let template = block.geometry.as_ref().ok_or_else(|| Issue { code: BlockIssue::Schema, path: "geometry".into() })?;
            ensure(valid_relative(&template.pointer), BlockIssue::InvalidPointer, "geometry/pointer")?;
            let template = GeometryRef { result_ref: None, ..template.clone() };
            for (position, (index, record)) in rows.records.iter().enumerate() {
                let path = format!("features/{position}");
                let label_fact = match block.label_pointer.as_deref() {
                    Some(pointer) if valid_relative(pointer) => Rows::cell(record, pointer, &format!("{path}/label_fact"))?,
                    Some(_) => return fail(BlockIssue::InvalidPointer, "label_pointer"),
                    None => None,
                };
                let geometry = geometry_ref(&template, locate(record, &template, &format!("{path}/geometry"))?);
                let feature = MapFeature { id: format!("row-{index}"), label: None, label_fact, geometry: None, facts: Vec::new(), row_index: Some(*index) };
                located.push(match geometry {
                    Some((geometry, crs, count)) => (MapFeature { geometry: Some(geometry), ..feature }, Some((crs, count))),
                    None => (feature, None),
                });
            }
            Some(rows.coverage)
        }
        None => {
            for (index, feature) in block.features.iter().enumerate() {
                let path = format!("features/{index}");
                scope.item_id(&feature.id, &path)?;
                if let Some(label) = &feature.label {
                    scope.label(label, &format!("{path}/label"))?;
                }
                let label_fact = scope.optional_fact(feature.label_fact.as_ref(), &format!("{path}/label_fact"))?;
                ensure(feature.label.is_some() || label_fact.is_some(), BlockIssue::Schema, format!("{path}/label"))?;
                let facts = feature.facts.iter().enumerate().map(|(i, item)| scope.labeled(item, &format!("{path}/facts/{i}"))).collect::<Checked<Vec<_>>>()?;
                let geometry = match &feature.geometry {
                    Some(template) => {
                        let geometry_path = format!("{path}/geometry");
                        let result_ref = template.result_ref.as_deref().ok_or_else(|| Issue { code: BlockIssue::UnknownReference, path: geometry_path.clone() })?;
                        let document = scope.document(result_ref, &geometry_path)?;
                        ensure(crate::report_assets::evidence_pointer(&template.pointer), BlockIssue::InvalidPointer, &geometry_path)?;
                        geometry_ref(template, locate(document, template, &geometry_path)?)
                    }
                    None => None,
                };
                let feature = MapFeature { label_fact, facts, geometry: None, row_index: None, ..feature.clone() };
                located.push(match geometry {
                    Some((geometry, crs, count)) => (MapFeature { geometry: Some(geometry), ..feature }, Some((crs, count))),
                    None => (feature, None),
                });
            }
            None
        }
    };
    ensure(located.len() <= MAX_MAP_FEATURES, BlockIssue::TooManyItems, "features")?;
    let coordinates: usize = located.iter().filter_map(|(_, placed)| placed.map(|(_, count)| count)).sum();
    ensure(coordinates <= MAX_MAP_COORDINATES, BlockIssue::TooManyItems, "features")?;
    let mut systems = located.iter().filter_map(|(_, placed)| placed.map(|(crs, _)| crs));
    let crs = systems.next();
    ensure(systems.all(|other| Some(other) == crs), BlockIssue::UnsupportedCrs, "features")?;
    let features = located.into_iter().map(|(feature, _)| feature).collect();
    Ok((MapBlock { features, crs, geometry: block.geometry.clone(), ..block }, coverage))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn template(pointer: &str, format: GeometryFormat) -> GeometryRef {
        GeometryRef { result_ref: None, pointer: pointer.into(), lon_pointer: None, format, crs_pointer: None, geometry: Value::Null, crs: None }
    }

    #[test]
    fn coordinates_come_from_the_source_in_a_proven_crs() {
        let record = json!({"centroide_ll":"POINT(4.89 52.37)","centroide_rd":"POINT(121000 487000)","geo":{"type":"Polygon","coordinates":[[[4.8,52.3],[4.9,52.3],[4.9,52.4],[4.8,52.3]]]},
            "rd":{"type":"Point","coordinates":[121000,487000]},"crs":"urn:ogc:def:crs:EPSG::28992","lat":52.37,"lon":4.89});
        let (geometry, crs, count) = locate(&record, &template("/centroide_ll", GeometryFormat::WktPoint), "g").unwrap().unwrap();
        assert_eq!((geometry, crs, count), (json!({"type":"Point","coordinates":[4.89,52.37]}), Crs::Epsg4326, 1));
        assert_eq!(locate(&record, &template("/centroide_rd", GeometryFormat::WktPoint), "g").unwrap().unwrap().1, Crs::Epsg28992);
        assert_eq!(locate(&record, &template("/geo", GeometryFormat::Geojson), "g").unwrap().unwrap().2, 4);
        let unproven = locate(&record, &template("/rd", GeometryFormat::Geojson), "g").unwrap_err();
        assert_eq!(unproven.code, BlockIssue::UnsupportedCrs, "projected coordinates need a stated CRS");
        let stated = GeometryRef { crs_pointer: Some("/crs".into()), ..template("/rd", GeometryFormat::Geojson) };
        assert_eq!(locate(&record, &stated, "g").unwrap().unwrap().1, Crs::Epsg28992);
        let pair = GeometryRef { lon_pointer: Some("/lon".into()), ..template("/lat", GeometryFormat::LatLon) };
        assert_eq!(locate(&record, &pair, "g").unwrap().unwrap().0, json!({"type":"Point","coordinates":[4.89,52.37]}));
        assert!(locate(&record, &template("/absent", GeometryFormat::Geojson), "g").unwrap().is_none(), "no location is a list entry, not a guess");
        assert_eq!(locate(&record, &template("/lat", GeometryFormat::Geojson), "g").unwrap_err().code, BlockIssue::InvalidGeometry);
    }
}
