//! OVERLAY — the transit overlays the client draws over the map, generated from
//! the region's OSM clip in pure Rust (ADR 0014). Region-driven
//! (`region.overlays`): each declared kind is built if implemented.
//! Skip-if-present; `FORCE_OVERLAY`.
//!
//! The *geometry* here is generic; the operator/network specifics (which
//! operator, which refs are metro lines, their colours, branch splits, GTFS
//! conventions, projection origin) live behind a [`TransitOverlayDriver`]
//! selected from the resolved region (ADR 0017). A region with no driver logs
//! and skips.
//!
//! `transit-lines` (concept doc 09 §3): for every driver-owned `route=subway|tram`
//! relation, union the track-way members across all direction/variant relations
//! of a line (shared track deduped by way id), emit one `MultiLineString` feature
//! per line with the GTFS route id + colour.
//!
//! `metro-stations` (concept doc 09 §2): per metro station, emit a `concourse`
//! (concave hull of the station's stop/platform/exit points), one `platform`
//! per direction-stop (a side strip offset along the real track), and an `exit`
//! per `subway_entrance`. Geometry is computed in local-planar metres (`geo`
//! concave hull, manual perpendicular offset — no shapely); the morphological
//! smoothing and corridor union of the reference impl are simplified here.

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use geo::{ConcaveHull, ConvexHull, MultiPoint, Point as GeoPoint};
use osmpbf::{Element, ElementReader};
use serde_json::{Value, json};

use crate::context::Context;
use crate::fsx;
use crate::regions::{LineKind, Projection, TransitOverlayDriver, overlay_driver};
use crate::step::Step;

const IMPLEMENTED: &[&str] = &["transit-lines", "metro-stations"];

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
        let Some(driver) = overlay_driver(ctx.country(), &ctx.region.id) else {
            tracing::info!(
                country = ctx.country(),
                city = %ctx.region.id,
                "no transit-overlay driver for this region; skipping overlays"
            );
            return Ok(());
        };
        let clip = ctx.graph_dir().join(ctx.clipped_osm_filename());
        anyhow::ensure!(
            clip.is_file(),
            "overlay needs the OSM clip; the CLIP step must run first"
        );
        tokio::fs::create_dir_all(ctx.output("output/overlays")).await?;

        for kind in kinds {
            match kind.as_str() {
                "transit-lines" => {
                    let gtfs = ctx.graph_dir().join(driver.gtfs_filename());
                    let clip = clip.clone();
                    let driver = Arc::clone(&driver);
                    // osmpbf is blocking; run the build off the async runtime.
                    let fc = tokio::task::spawn_blocking(move || {
                        build_transit_lines(&clip, &gtfs, driver.as_ref())
                    })
                    .await??;
                    let bytes = serde_json::to_vec(&fc)?;
                    fsx::write_atomic(&ctx.output("output/overlays/transit-lines.geojson"), &bytes)
                        .await?;
                    tracing::info!(
                        features = fc["features"].as_array().map(Vec::len).unwrap_or(0),
                        "wrote transit-lines overlay"
                    );
                }
                "metro-stations" => {
                    let clip = clip.clone();
                    let driver = Arc::clone(&driver);
                    let fc = tokio::task::spawn_blocking(move || {
                        build_metro_stations(&clip, driver.as_ref())
                    })
                    .await??;
                    let bytes = serde_json::to_vec(&fc)?;
                    fsx::write_atomic(
                        &ctx.output("output/overlays/metro-stations.geojson"),
                        &bytes,
                    )
                    .await?;
                    tracing::info!(
                        features = fc["features"].as_array().map(Vec::len).unwrap_or(0),
                        "wrote metro-stations overlay"
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

/// The GeoJSON `kind` label for a route line.
fn line_kind_str(kind: LineKind) -> &'static str {
    match kind {
        LineKind::Metro => "metro",
        LineKind::Tram => "tram",
    }
}

struct RouteRel {
    kind: LineKind,
    line: String,
    way_ids: Vec<i64>,
}

/// Build the transit-lines FeatureCollection from the OSM clip + (optional) GTFS.
fn build_transit_lines(
    clip: &Path,
    gtfs: &Path,
    driver: &dyn TransitOverlayDriver,
) -> anyhow::Result<Value> {
    let rels = collect_route_relations(clip, driver)?;
    let needed_ways: HashSet<i64> = rels
        .iter()
        .flat_map(|r| r.way_ids.iter().copied())
        .collect();
    let way_nodes = collect_ways(clip, &needed_ways)?;
    let needed_nodes: HashSet<i64> = way_nodes.values().flatten().copied().collect();
    let node_xy = collect_nodes(clip, &needed_nodes)?;
    let gtfs_routes = read_gtfs_routes(gtfs).unwrap_or_default();

    // Group relations by (kind, line); union their track ways (dedup by id).
    let mut groups: HashMap<(LineKind, String), Vec<i64>> = HashMap::new();
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

        let gkey = driver.gtfs_key(kind, &line);
        let g = gtfs_routes.get(&gkey);
        let route = format!(
            "{}{}",
            driver.route_id_prefix(),
            g.map(|r| r.id.clone()).unwrap_or_else(|| gkey.clone())
        );
        let color = match kind {
            LineKind::Metro => Some(
                g.and_then(|r| (!r.color.is_empty()).then(|| format!("#{}", r.color)))
                    .unwrap_or_else(|| driver.metro_color(&line).to_string()),
            ),
            LineKind::Tram => None,
        };

        features.push(json!({
            "type": "Feature",
            "properties": { "kind": line_kind_str(kind), "route": route, "line": line, "color": color },
            "geometry": { "type": "MultiLineString", "coordinates": members },
        }));
    }

    sort_metro_first(&mut features);
    Ok(json!({ "type": "FeatureCollection", "features": features }))
}

/// Pass 1: the driver-owned `route=subway|tram` relations with a ref, and their
/// track-way members (members whose role doesn't start with `platform`).
fn collect_route_relations(
    clip: &Path,
    driver: &dyn TransitOverlayDriver,
) -> anyhow::Result<Vec<RouteRel>> {
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
            if operator.as_deref() != Some(driver.operator()) || line.is_empty() {
                return;
            }
            let kind = match route.as_str() {
                "subway" if driver.is_metro_line(&line) => LineKind::Metro,
                "tram" => LineKind::Tram,
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

// ─── metro-stations (concept doc 09 §2) ──────────────────────────────────────

// Metres per degree latitude — constant everywhere; the longitude scale and the
// projection origin are region-specific and come from the driver.
const M_LAT: f64 = 111_320.0;
const HALF_LEN_M: f64 = 52.0; // platform half-length along the track
const DENSIFY_M: f64 = 16.0;
const PLAT_GAP_M: f64 = 1.2; // offset from the track centreline to the near edge
const PLAT_WIDTH_M: f64 = 5.0;
const EXIT_ASSIGN_M: f64 = 400.0;
const HULL_CONCAVITY_M: f64 = 60.0;

fn to_m(proj: Projection, lon: f64, lat: f64) -> (f64, f64) {
    (
        (lon - proj.origin_lon) * proj.m_per_deg_lon,
        (lat - proj.origin_lat) * M_LAT,
    )
}
fn from_m(proj: Projection, x: f64, y: f64) -> [f64; 2] {
    [
        round7(proj.origin_lon + x / proj.m_per_deg_lon),
        round7(proj.origin_lat + y / M_LAT),
    ]
}

struct MetroRel {
    line: String,
    stop_ids: Vec<i64>,
}
struct StopNode {
    name: String,
    x: f64,
    y: f64,
}
struct Entrance {
    name: String,
    x: f64,
    y: f64,
}

/// The resolved nodes for the metro-stations build.
struct MetroNodes {
    stops: HashMap<i64, StopNode>,
    entrances: Vec<Entrance>,
    node_xy: HashMap<i64, (f64, f64)>,
}

/// Build the metro-stations FeatureCollection from the OSM clip.
fn build_metro_stations(clip: &Path, driver: &dyn TransitOverlayDriver) -> anyhow::Result<Value> {
    let proj = driver.projection();
    let mut rels = collect_metro_relations(clip, driver)?;
    let stop_ids: HashSet<i64> = rels
        .iter()
        .flat_map(|r| r.stop_ids.iter().copied())
        .collect();
    let (track_node_lists, track_node_ids) = collect_track_ways(clip)?;
    let MetroNodes {
        stops,
        entrances,
        node_xy,
    } = collect_metro_nodes(clip, proj, &stop_ids, &track_node_ids)?;

    // Real track polylines in local metres (for the platform strips).
    let tracks: Vec<Vec<(f64, f64)>> = track_node_lists
        .iter()
        .map(|ids| {
            ids.iter()
                .filter_map(|n| node_xy.get(n).copied())
                .collect::<Vec<_>>()
        })
        .filter(|p: &Vec<(f64, f64)>| p.len() >= 2)
        .collect();

    // Branch split (driver-owned): relabel a line to its branch from the slugs of
    // the stops it serves (e.g. Rome's B → B1 for the Jonio / Conca d'Oro spur).
    for r in &mut rels {
        let stop_slugs: Vec<String> = r
            .stop_ids
            .iter()
            .filter_map(|id| stops.get(id))
            .map(|s| slug(&s.name))
            .collect();
        if let Some(branch) = driver.relabel_branch(&r.line, &stop_slugs) {
            r.line = branch;
        }
    }

    let mut platforms = Vec::new();
    // station slug → points feeding its concourse hull
    let mut hull_pts: HashMap<String, Vec<(f64, f64)>> = HashMap::new();

    for r in &rels {
        let terminus = r
            .stop_ids
            .last()
            .and_then(|id| stops.get(id))
            .map(|s| s.name.clone());
        for id in &r.stop_ids {
            let Some(stop) = stops.get(id) else { continue };
            let slug = slug(&stop.name);
            hull_pts
                .entry(slug.clone())
                .or_default()
                .push((stop.x, stop.y));

            if let Some(ring) = platform_ring(&tracks, (stop.x, stop.y)) {
                hull_pts
                    .entry(slug.clone())
                    .or_default()
                    .extend(ring.iter().copied());
                let coords: Vec<Value> = ring
                    .iter()
                    .map(|(x, y)| {
                        Value::Array(from_m(proj, *x, *y).iter().map(|v| json!(v)).collect())
                    })
                    .collect();
                let mut props = json!({
                    "kind": "platform", "station": slug, "line": r.line,
                    "color": driver.metro_color(&r.line), "level": -1,
                });
                if let Some(t) = &terminus {
                    props["name"] = json!(format!("dir. {t}"));
                }
                platforms.push(json!({
                    "type": "Feature", "properties": props,
                    "geometry": { "type": "Polygon", "coordinates": [coords] },
                }));
            }
        }
    }

    // Exits: assign each entrance to the nearest station slug within range.
    let centroids = station_centroids(&hull_pts);
    let mut exits = Vec::new();
    for e in &entrances {
        if let Some((slug, d2)) = nearest_station(&centroids, e.x, e.y)
            && d2 <= EXIT_ASSIGN_M * EXIT_ASSIGN_M
        {
            hull_pts.entry(slug.clone()).or_default().push((e.x, e.y));
            exits.push(json!({
                "type": "Feature",
                "properties": { "kind": "exit", "station": slug, "name": e.name },
                "geometry": { "type": "Point", "coordinates": from_m(proj, e.x, e.y) },
            }));
        }
    }

    // Concourses: concave hull of each station's points. Emit order: all
    // concourses, then all platforms, then all exits.
    let mut slugs: Vec<&String> = hull_pts.keys().collect();
    slugs.sort();
    let mut features = Vec::new();
    for slug in slugs {
        if let Some(ring) = station_hull(proj, &hull_pts[slug]) {
            features.push(json!({
                "type": "Feature",
                "properties": { "kind": "concourse", "station": slug, "level": 0 },
                "geometry": { "type": "Polygon", "coordinates": [ring] },
            }));
        }
    }
    features.extend(platforms);
    features.extend(exits);
    Ok(json!({ "type": "FeatureCollection", "features": features }))
}

/// Pass A: the driver's metro `route=subway` relations with their ordered stop
/// node members (role starting `stop`).
fn collect_metro_relations(
    clip: &Path,
    driver: &dyn TransitOverlayDriver,
) -> anyhow::Result<Vec<MetroRel>> {
    let mut out = Vec::new();
    ElementReader::from_path(clip)?.for_each(|el| {
        if let Element::Relation(rel) = el {
            let mut route = None;
            let mut line = None;
            for (k, v) in rel.tags() {
                match k {
                    "route" => route = Some(v.to_string()),
                    "ref" => line = Some(v.to_string()),
                    _ => {}
                }
            }
            let (Some(route), Some(line)) = (route, line) else {
                return;
            };
            if route != "subway" || !driver.is_metro_line(&line) {
                return;
            }
            let stop_ids: Vec<i64> = rel
                .members()
                .filter(|m| {
                    m.member_type == osmpbf::RelMemberType::Node
                        && m.role().unwrap_or("").starts_with("stop")
                })
                .map(|m| m.member_id)
                .collect();
            if !stop_ids.is_empty() {
                out.push(MetroRel { line, stop_ids });
            }
        }
    })?;
    Ok(out)
}

/// Pass B: `railway=subway` track ways → their node-id lists + the node-id set.
fn collect_track_ways(clip: &Path) -> anyhow::Result<(Vec<Vec<i64>>, HashSet<i64>)> {
    let mut lists = Vec::new();
    let mut ids = HashSet::new();
    ElementReader::from_path(clip)?.for_each(|el| {
        if let Element::Way(way) = el
            && way.tags().any(|(k, v)| k == "railway" && v == "subway")
        {
            let nodes: Vec<i64> = way.refs().collect();
            ids.extend(nodes.iter().copied());
            lists.push(nodes);
        }
    })?;
    Ok((lists, ids))
}

/// Pass C: resolve stop nodes (name + coords), `subway_entrance` nodes, and the
/// track nodes' coords.
fn collect_metro_nodes(
    clip: &Path,
    proj: Projection,
    stop_ids: &HashSet<i64>,
    track_ids: &HashSet<i64>,
) -> anyhow::Result<MetroNodes> {
    let mut stops = HashMap::new();
    let mut entrances = Vec::new();
    let mut node_xy = HashMap::new();

    let mut handle = |id: i64, lon: f64, lat: f64, tag: &dyn Fn(&str) -> Option<String>| {
        let (x, y) = to_m(proj, lon, lat);
        if track_ids.contains(&id) {
            node_xy.insert(id, (x, y));
        }
        if stop_ids.contains(&id) {
            let name = tag("name").unwrap_or_default();
            stops.insert(id, StopNode { name, x, y });
        }
        if tag("railway").as_deref() == Some("subway_entrance") {
            let name = tag("name")
                .or_else(|| tag("ref").map(|r| format!("Uscita {r}")))
                .unwrap_or_else(|| "Uscita".to_string());
            entrances.push(Entrance { name, x, y });
        }
    };

    ElementReader::from_path(clip)?.for_each(|el| match el {
        Element::Node(n) => {
            let tag = |k: &str| {
                n.tags()
                    .find(|(tk, _)| *tk == k)
                    .map(|(_, v)| v.to_string())
            };
            handle(n.id(), n.lon(), n.lat(), &tag);
        }
        Element::DenseNode(n) => {
            let tag = |k: &str| {
                n.tags()
                    .find(|(tk, _)| *tk == k)
                    .map(|(_, v)| v.to_string())
            };
            handle(n.id(), n.lon(), n.lat(), &tag);
        }
        _ => {}
    })?;
    Ok(MetroNodes {
        stops,
        entrances,
        node_xy,
    })
}

/// A side-platform ring (metres): the ±`HALF_LEN_M` window of the nearest track
/// around the stop, densified, offset perpendicular to one side by
/// `[GAP, GAP+WIDTH]` and closed (near edge forward, far edge reversed).
fn platform_ring(tracks: &[Vec<(f64, f64)>], stop: (f64, f64)) -> Option<Vec<(f64, f64)>> {
    // Nearest track + nearest vertex on it.
    let (track, idx) = tracks
        .iter()
        .filter_map(|t| {
            t.iter()
                .enumerate()
                .map(|(i, p)| (i, dist2(*p, stop)))
                .min_by(|a, b| a.1.total_cmp(&b.1))
                .map(|(i, d2)| (t, i, d2))
        })
        .min_by(|a, b| a.2.total_cmp(&b.2))
        .map(|(t, i, _)| (t, i))?;

    // ±HALF_LEN window along the polyline.
    let mut lo = idx;
    let mut hi = idx;
    let mut len = 0.0;
    while lo > 0 && len < HALF_LEN_M {
        len += dist2(track[lo], track[lo - 1]).sqrt();
        lo -= 1;
    }
    len = 0.0;
    while hi + 1 < track.len() && len < HALF_LEN_M {
        len += dist2(track[hi], track[hi + 1]).sqrt();
        hi += 1;
    }
    let window = densify(&track[lo..=hi]);
    if window.len() < 2 {
        return None;
    }

    // Per-point left normal (average of adjacent segment normals).
    let n = window.len();
    let mut near = Vec::with_capacity(n);
    let mut far = Vec::with_capacity(n);
    for i in 0..n {
        let a = window[i.saturating_sub(1)];
        let b = window[(i + 1).min(n - 1)];
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let m = (dx * dx + dy * dy).sqrt().max(1e-6);
        let (nx, ny) = (-dy / m, dx / m); // left normal
        let p = window[i];
        near.push((p.0 + nx * PLAT_GAP_M, p.1 + ny * PLAT_GAP_M));
        far.push((
            p.0 + nx * (PLAT_GAP_M + PLAT_WIDTH_M),
            p.1 + ny * (PLAT_GAP_M + PLAT_WIDTH_M),
        ));
    }
    let mut ring = near;
    ring.extend(far.into_iter().rev());
    let first = ring[0];
    ring.push(first);
    Some(ring)
}

fn densify(line: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut out = Vec::new();
    for w in line.windows(2) {
        let (a, b) = (w[0], w[1]);
        out.push(a);
        let d = dist2(a, b).sqrt();
        let steps = (d / DENSIFY_M).floor() as usize;
        for s in 1..steps {
            let t = s as f64 / steps as f64;
            out.push((a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t));
        }
    }
    if let Some(last) = line.last() {
        out.push(*last);
    }
    out
}

fn station_centroids(hull_pts: &HashMap<String, Vec<(f64, f64)>>) -> Vec<(String, f64, f64)> {
    hull_pts
        .iter()
        .map(|(slug, pts)| {
            let n = pts.len() as f64;
            let (sx, sy) = pts
                .iter()
                .fold((0.0, 0.0), |(ax, ay), (x, y)| (ax + x, ay + y));
            (slug.clone(), sx / n, sy / n)
        })
        .collect()
}

fn nearest_station(centroids: &[(String, f64, f64)], x: f64, y: f64) -> Option<(String, f64)> {
    centroids
        .iter()
        .map(|(s, cx, cy)| (s.clone(), dist2((*cx, *cy), (x, y))))
        .min_by(|a, b| a.1.total_cmp(&b.1))
}

/// The concave hull of a station's points as a lon/lat ring (convex-hull
/// fallback for sparse stations).
fn station_hull(proj: Projection, pts: &[(f64, f64)]) -> Option<Vec<Value>> {
    if pts.len() < 3 {
        return None;
    }
    let mp = MultiPoint::from(
        pts.iter()
            .map(|(x, y)| GeoPoint::new(*x, *y))
            .collect::<Vec<_>>(),
    );
    let hull = mp.concave_hull(HULL_CONCAVITY_M);
    let ext: Vec<(f64, f64)> = hull.exterior().points().map(|p| (p.x(), p.y())).collect();
    let ring = if ext.len() >= 4 {
        ext
    } else {
        mp.convex_hull()
            .exterior()
            .points()
            .map(|p| (p.x(), p.y()))
            .collect()
    };
    if ring.len() < 4 {
        return None;
    }
    Some(
        ring.iter()
            .map(|(x, y)| Value::Array(from_m(proj, *x, *y).iter().map(|v| json!(v)).collect()))
            .collect(),
    )
}

fn dist2(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (dx, dy) = (a.0 - b.0, a.1 - b.1);
    dx * dx + dy * dy
}

/// Accent-stripped, lowercased, alnum slug for grouping a station's features.
fn slug(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for ch in name.chars() {
        let c = match ch.to_ascii_lowercase() {
            'à' | 'á' | 'â' => 'a',
            'è' | 'é' | 'ê' => 'e',
            'ì' | 'í' => 'i',
            'ò' | 'ó' => 'o',
            'ù' | 'ú' => 'u',
            c => c,
        };
        if c.is_ascii_alphanumeric() {
            out.push(c);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    out.trim_matches('-').to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::regions::italy::rome::RomeOverlayDriver;

    /// The local-planar projection the generic geometry tests run in (Rome's, via
    /// the driver) — the metre math is generic; we just need a concrete origin.
    fn test_proj() -> Projection {
        RomeOverlayDriver.projection()
    }

    #[test]
    fn slug_strips_accents_and_punctuation() {
        assert_eq!(slug("San Giovanni"), "san-giovanni");
        assert_eq!(slug("Conca d'Oro"), "conca-d-oro");
        assert_eq!(slug("Niccolò"), "niccolo");
        assert_eq!(slug("Termini"), "termini");
    }

    #[test]
    fn platform_ring_is_a_closed_strip_along_the_track() {
        // a straight 200 m track in metres (east-west), stop at the middle.
        let track: Vec<(f64, f64)> = (0..=20).map(|i| (i as f64 * 10.0, 0.0)).collect();
        let ring = platform_ring(&[track], (100.0, 0.0)).unwrap();
        assert!(ring.len() >= 4);
        assert_eq!(ring.first(), ring.last(), "ring is closed");
        // all points are within ~HALF_LEN of the stop and offset to one side.
        let ys: Vec<f64> = ring.iter().map(|(_, y)| *y).collect();
        assert!(ys.iter().all(|y| *y >= 0.0)); // offset to the +y (left) side only
        assert!(ys.iter().cloned().fold(0.0_f64, f64::max) >= PLAT_GAP_M + PLAT_WIDTH_M - 0.1);
    }

    #[test]
    fn station_hull_wraps_the_points() {
        let pts = vec![
            (0.0, 0.0),
            (100.0, 0.0),
            (100.0, 100.0),
            (0.0, 100.0),
            (50.0, 50.0),
        ];
        let ring = station_hull(test_proj(), &pts).unwrap();
        assert!(ring.len() >= 4, "a hull ring");
        // ring coordinates are [lon, lat] back in WGS84 near the projection origin.
        let lon = ring[0][0].as_f64().unwrap();
        assert!((12.0..13.0).contains(&lon));
    }

    #[test]
    fn round7_rounds_to_seven_places() {
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
