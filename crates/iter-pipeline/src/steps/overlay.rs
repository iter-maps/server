//! OVERLAY — the transit overlays the client draws over the map, generated from
//! the region's OSM clip. Region-driven (`region.overlays`): each declared kind
//! is built if implemented. `transit-lines` is built here (pure-Rust, no
//! geometry library — it's an id-level union of OSM track ways); `metro-stations`
//! (platform/concourse geometry) lands next. Skip-if-present; `FORCE_OVERLAY`.
//!
//! transit-lines (concept doc 09 §3): for every `route=subway|tram` relation
//! operated by ATAC, union the track-way members across all direction/variant
//! relations of a line (shared track deduped by way id), emit one
//! `MultiLineString` feature per line with the GTFS route id + colour. So all
//! four `ref=8` variants collapse to one line covering each track once.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;

use async_trait::async_trait;
use osmpbf::{Element, ElementReader};
use serde_json::{Value, json};

use crate::context::Context;
use crate::fsx;
use crate::step::Step;

const IMPLEMENTED: &[&str] = &["transit-lines"];

pub struct BuildOverlays;

#[async_trait]
impl Step for BuildOverlays {
    fn name(&self) -> &'static str {
        "OVERLAY"
    }

    async fn satisfied(&self, ctx: &Context) -> bool {
        for kind in declared_implemented(ctx) {
            let out = ctx.output(&format!("output/overlays/{kind}.geojson"));
            if !tokio::fs::metadata(&out)
                .await
                .map(|m| m.len() > 0)
                .unwrap_or(false)
            {
                return false;
            }
        }
        true
    }

    async fn run(&self, ctx: &Context) -> anyhow::Result<()> {
        let kinds = declared_implemented(ctx);
        if kinds.is_empty() {
            tracing::info!("region declares no implemented overlays; skipping");
            return Ok(());
        }
        let clip = ctx.graph_dir().join(ctx.clipped_osm_filename());
        anyhow::ensure!(
            clip.is_file(),
            "overlay needs the OSM clip; the CLIP step must run first"
        );
        tokio::fs::create_dir_all(ctx.output("output/overlays")).await?;

        for kind in kinds {
            match kind.as_str() {
                "transit-lines" => {
                    let gtfs = ctx.graph_dir().join("ATAC.gtfs.zip");
                    let clip = clip.clone();
                    // osmpbf is blocking; run the build off the async runtime.
                    let fc = tokio::task::spawn_blocking(move || build_transit_lines(&clip, &gtfs))
                        .await??;
                    let bytes = serde_json::to_vec(&fc)?;
                    fsx::write_atomic(&ctx.output("output/overlays/transit-lines.geojson"), &bytes)
                        .await?;
                    tracing::info!(
                        features = fc["features"].as_array().map(Vec::len).unwrap_or(0),
                        "wrote transit-lines overlay"
                    );
                }
                other => tracing::warn!(kind = other, "overlay kind not implemented; skipping"),
            }
        }
        Ok(())
    }
}

/// The region's declared overlay kinds this step can build.
fn declared_implemented(ctx: &Context) -> Vec<String> {
    ctx.region
        .overlays
        .iter()
        .map(|o| o.kind.clone())
        .filter(|k| IMPLEMENTED.contains(&k.as_str()))
        .collect()
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum Kind {
    Metro,
    Tram,
}

impl Kind {
    fn as_str(self) -> &'static str {
        match self {
            Kind::Metro => "metro",
            Kind::Tram => "tram",
        }
    }
}

struct RouteRel {
    kind: Kind,
    line: String,
    way_ids: Vec<i64>,
}

/// Build the transit-lines FeatureCollection from the OSM clip + (optional) GTFS.
fn build_transit_lines(clip: &Path, gtfs: &Path) -> anyhow::Result<Value> {
    let rels = collect_route_relations(clip)?;
    let needed_ways: HashSet<i64> = rels
        .iter()
        .flat_map(|r| r.way_ids.iter().copied())
        .collect();
    let way_nodes = collect_ways(clip, &needed_ways)?;
    let needed_nodes: HashSet<i64> = way_nodes.values().flatten().copied().collect();
    let node_xy = collect_nodes(clip, &needed_nodes)?;
    let gtfs_routes = read_gtfs_routes(gtfs).unwrap_or_default();

    // Group relations by (kind, line); union their track ways (dedup by id).
    let mut groups: HashMap<(Kind, String), Vec<i64>> = HashMap::new();
    for r in rels {
        groups
            .entry((r.kind, r.line.clone()))
            .or_default()
            .extend(r.way_ids);
    }

    let mut features = Vec::new();
    for ((kind, line), way_ids) in groups {
        let mut seen = HashSet::new();
        let mut members = Vec::new();
        for w in way_ids {
            if !seen.insert(w) {
                continue; // shared track counted once (bidirectional dedup)
            }
            if let Some(nodes) = way_nodes.get(&w) {
                let coords: Vec<Value> = nodes
                    .iter()
                    .filter_map(|n| node_xy.get(n))
                    .map(|[lon, lat]| json!([round7(*lon), round7(*lat)]))
                    .collect();
                if coords.len() >= 2 {
                    members.push(Value::Array(coords));
                }
            }
        }
        if members.is_empty() {
            continue;
        }

        let gkey = match kind {
            Kind::Metro => format!("ME{line}"),
            Kind::Tram => line.clone(),
        };
        let g = gtfs_routes.get(&gkey);
        let route = format!(
            "ATAC:{}",
            g.map(|r| r.id.clone()).unwrap_or_else(|| gkey.clone())
        );
        let color = match kind {
            Kind::Metro => Some(
                g.and_then(|r| (!r.color.is_empty()).then(|| format!("#{}", r.color)))
                    .unwrap_or_else(|| contract_metro_color(&line).to_string()),
            ),
            Kind::Tram => None,
        };

        features.push(json!({
            "type": "Feature",
            "properties": { "kind": kind.as_str(), "route": route, "line": line, "color": color },
            "geometry": { "type": "MultiLineString", "coordinates": members },
        }));
    }

    sort_metro_first(&mut features);
    Ok(json!({ "type": "FeatureCollection", "features": features }))
}

/// Pass 1: ATAC `route=subway|tram` relations with a ref, and their track-way
/// members (members whose role doesn't start with `platform`).
fn collect_route_relations(clip: &Path) -> anyhow::Result<Vec<RouteRel>> {
    let mut out = Vec::new();
    ElementReader::from_path(clip)?.for_each(|el| {
        if let Element::Relation(rel) = el {
            let mut route = None;
            let mut line = None;
            let mut operator = None;
            for (k, v) in rel.tags() {
                match k {
                    "route" => route = Some(v.to_string()),
                    "ref" => line = Some(v.to_string()),
                    "operator" => operator = Some(v.to_string()),
                    _ => {}
                }
            }
            let (Some(route), Some(line)) = (route, line) else {
                return;
            };
            if operator.as_deref() != Some("ATAC") || line.is_empty() {
                return;
            }
            let kind = match route.as_str() {
                "subway" if matches!(line.as_str(), "A" | "B" | "C") => Kind::Metro,
                "tram" => Kind::Tram,
                _ => return,
            };
            let way_ids: Vec<i64> = rel
                .members()
                .filter(|m| {
                    m.member_type == osmpbf::RelMemberType::Way
                        && !m.role().unwrap_or("").starts_with("platform")
                })
                .map(|m| m.member_id)
                .collect();
            if !way_ids.is_empty() {
                out.push(RouteRel {
                    kind,
                    line,
                    way_ids,
                });
            }
        }
    })?;
    Ok(out)
}

/// Pass 2: for the needed way ids, their ordered node refs.
fn collect_ways(clip: &Path, needed: &HashSet<i64>) -> anyhow::Result<HashMap<i64, Vec<i64>>> {
    let mut out = HashMap::new();
    ElementReader::from_path(clip)?.for_each(|el| {
        if let Element::Way(way) = el
            && needed.contains(&way.id())
        {
            out.insert(way.id(), way.refs().collect());
        }
    })?;
    Ok(out)
}

/// Pass 3: for the needed node ids, their `[lon, lat]`.
fn collect_nodes(clip: &Path, needed: &HashSet<i64>) -> anyhow::Result<HashMap<i64, [f64; 2]>> {
    let mut out = HashMap::new();
    ElementReader::from_path(clip)?.for_each(|el| match el {
        Element::Node(n) if needed.contains(&n.id()) => {
            out.insert(n.id(), [n.lon(), n.lat()]);
        }
        Element::DenseNode(n) if needed.contains(&n.id()) => {
            out.insert(n.id(), [n.lon(), n.lat()]);
        }
        _ => {}
    })?;
    Ok(out)
}

struct GtfsRoute {
    id: String,
    color: String,
}

/// Read `routes.txt` from the GTFS zip → `short_name → {route_id, color}` for
/// metro/tram routes. Fail-soft: a missing zip yields an empty map.
fn read_gtfs_routes(gtfs: &Path) -> anyhow::Result<HashMap<String, GtfsRoute>> {
    let file = std::fs::File::open(gtfs)?;
    let mut zip = zip::ZipArchive::new(file)?;
    let mut text = String::new();
    zip.by_name("routes.txt")?.read_to_string(&mut text)?;

    let mut lines = text.lines();
    let header: Vec<&str> = lines
        .next()
        .unwrap_or("")
        .split(',')
        .map(str::trim)
        .collect();
    let col = |name: &str| header.iter().position(|h| h.trim_matches('"') == name);
    let (Some(id_i), Some(short_i), Some(type_i)) =
        (col("route_id"), col("route_short_name"), col("route_type"))
    else {
        return Ok(HashMap::new());
    };
    let color_i = col("route_color");

    let mut out = HashMap::new();
    for row in lines {
        let f: Vec<&str> = row.split(',').map(|s| s.trim().trim_matches('"')).collect();
        let get = |i: usize| f.get(i).copied().unwrap_or("");
        // route_type 0 = tram, 1 = metro/subway.
        if !matches!(get(type_i), "0" | "1") {
            continue;
        }
        let short = get(short_i).to_string();
        if short.is_empty() {
            continue;
        }
        out.insert(
            short,
            GtfsRoute {
                id: get(id_i).to_string(),
                color: color_i.map(get).unwrap_or("").to_string(),
            },
        );
    }
    Ok(out)
}

fn contract_metro_color(line: &str) -> &'static str {
    match line {
        "A" => "#E27439",
        "B" => "#0570B5",
        "C" => "#008456",
        _ => "#666666",
    }
}

/// Metro first (A, B, C), then trams ascending by number.
fn sort_metro_first(features: &mut [Value]) {
    features.sort_by(|a, b| {
        let key = |f: &Value| {
            let kind = f["properties"]["kind"].as_str().unwrap_or("");
            let line = f["properties"]["line"].as_str().unwrap_or("").to_string();
            let is_tram = kind == "tram";
            let num = line.parse::<i64>().unwrap_or(i64::MAX);
            (is_tram, num, line)
        };
        key(a).cmp(&key(b))
    });
}

fn round7(v: f64) -> f64 {
    (v * 1e7).round() / 1e7
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_colors_and_rounding() {
        assert_eq!(contract_metro_color("A"), "#E27439");
        assert_eq!(contract_metro_color("B"), "#0570B5");
        assert_eq!(round7(12.123456789), 12.1234568);
    }

    #[test]
    fn metro_sorts_before_trams_then_numeric() {
        let mut f = vec![
            json!({"properties":{"kind":"tram","line":"14"}}),
            json!({"properties":{"kind":"metro","line":"B"}}),
            json!({"properties":{"kind":"tram","line":"2"}}),
            json!({"properties":{"kind":"metro","line":"A"}}),
        ];
        sort_metro_first(&mut f);
        let order: Vec<&str> = f
            .iter()
            .map(|x| x["properties"]["line"].as_str().unwrap())
            .collect();
        assert_eq!(order, ["A", "B", "2", "14"]);
    }

    #[test]
    fn gtfs_routes_parsed_metro_tram_only() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("g.zip");
        let f = std::fs::File::create(&path).unwrap();
        let mut z = zip::ZipWriter::new(f);
        use std::io::Write;
        z.start_file::<_, ()>("routes.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        z.write_all(
            b"route_id,route_short_name,route_type,route_color\nMEA,MEA,1,E27439\nR8,8,0,\nBUS,100,3,FF0000\n",
        )
        .unwrap();
        z.finish().unwrap();

        let routes = read_gtfs_routes(&path).unwrap();
        assert_eq!(routes.get("MEA").unwrap().id, "MEA");
        assert_eq!(routes.get("MEA").unwrap().color, "E27439");
        assert!(routes.contains_key("8")); // tram kept
        assert!(!routes.contains_key("100")); // bus (route_type 3) dropped
    }

    #[test]
    fn missing_gtfs_is_fail_soft() {
        assert!(read_gtfs_routes(Path::new("/no/such.zip")).is_err());
        // build_transit_lines treats the error as an empty map (unwrap_or_default).
    }
}
