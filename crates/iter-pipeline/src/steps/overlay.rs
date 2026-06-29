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
//! `transit-lines`: for every driver-owned route relation in the widened set
//! (`route=subway|tram|light_rail|rail|regional_rail`, ADR 0029), union the
//! track-way members across all direction/variant relations of a line (shared
//! track deduped by way id), emit one `MultiLineString` feature per line with the
//! GTFS route id + colour. Member route relations gathered under one
//! `route_master` collapse to a single line; otherwise lines group by
//! `(network, route mode, ref)`. Each feature carries additive `network` +
//! `routable` identity props (ADR 0029); overlay-only lines get an
//! `OSM:<network>:<ref>` id.
//!
//! `metro-stations`: per metro station, emit a `concourse` (concave hull of the
//! station's stop/platform/exit points, smoothed into an organic footprint via
//! Chaikin corner-cutting + Visvalingam-Whyatt simplification, then the
//! concourse dissolved with its overlapping platform strips into one footprint —
//! ADR 0031), one `platform` per direction-stop (a side strip offset along the
//! real track), and an `exit` per `subway_entrance`.
//! Geometry is computed in local-planar metres (`geo` concave hull, manual
//! perpendicular offset).

use std::collections::{HashMap, HashSet};
use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use geo::coordinate_position::CoordPos;
use geo::{
    Area, Centroid, ConcaveHull, ConvexHull, CoordinatePosition, LineString, MultiPoint,
    Point as GeoPoint, Polygon, SimplifyVwPreserve, Validation, coord, unary_union,
};
use osmpbf::{Element, ElementReader};
use serde_json::{Value, json};

use iter_region_drivers::{LineKind, Projection, TransitOverlayDriver, overlay_driver};

use crate::context::Context;
use crate::fsx;
use crate::step::Step;
use crate::steps::IMPLEMENTED_OVERLAY_KINDS as IMPLEMENTED;

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

/// The OSM `route=*` modes the overlay draws (ADR 0029). Metro and tram keep the
/// original driver-dispatched behaviour (the driver's [`LineKind`] colour/gtfs
/// conventions); the widened rail family draws as generic lines (null colour,
/// OSM-derived id) where a region has no in-scope timetable for them.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
enum RouteMode {
    Metro,
    Tram,
    LightRail,
    Rail,
    RegionalRail,
}

impl RouteMode {
    /// Map an OSM `route=*` value to a draw mode. `subway` only counts as metro
    /// when the driver's allow-set promotes it; everything else maps by name.
    fn from_osm(route: &str, line: &str, driver: &dyn TransitOverlayDriver) -> Option<Self> {
        match route {
            "subway" if driver.is_metro_line(line) => Some(RouteMode::Metro),
            "tram" => Some(RouteMode::Tram),
            "light_rail" => Some(RouteMode::LightRail),
            "rail" => Some(RouteMode::Rail),
            "regional_rail" => Some(RouteMode::RegionalRail),
            _ => None,
        }
    }

    /// The GeoJSON `kind` label.
    fn kind_str(self) -> &'static str {
        match self {
            RouteMode::Metro => "metro",
            RouteMode::Tram => "tram",
            RouteMode::LightRail => "light_rail",
            RouteMode::Rail => "rail",
            RouteMode::RegionalRail => "regional_rail",
        }
    }

    /// The driver [`LineKind`] this mode dispatches through, if any. Only metro
    /// and tram carry driver-owned colour/gtfs conventions; the rail family draws
    /// generically.
    fn line_kind(self) -> Option<LineKind> {
        match self {
            RouteMode::Metro => Some(LineKind::Metro),
            RouteMode::Tram => Some(LineKind::Tram),
            _ => None,
        }
    }
}

struct RouteRel {
    mode: RouteMode,
    /// The OSM `network` (preferred) or `operator` tag — the line's network id.
    network: String,
    line: String,
    way_ids: Vec<i64>,
    /// The `route_master` relation id grouping this route's variants, if any.
    master: Option<i64>,
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

    // Group relations into lines. A `route_master` (the master id) collapses its
    // member route variants into one line; otherwise the line keys on
    // (network, mode, ref). The grouped line takes its mode/network/ref from the
    // member relations (route_master itself carries no track).
    #[derive(PartialEq, Eq, Hash)]
    enum GroupKey {
        Master(i64),
        Fallback(String, RouteMode, String),
    }
    struct Line {
        mode: RouteMode,
        network: String,
        line: String,
        way_ids: Vec<i64>,
    }
    let mut groups: HashMap<GroupKey, Line> = HashMap::new();
    for r in rels {
        let key = match r.master {
            Some(id) => GroupKey::Master(id),
            None => GroupKey::Fallback(r.network.clone(), r.mode, r.line.clone()),
        };
        let entry = groups.entry(key).or_insert_with(|| Line {
            mode: r.mode,
            network: r.network.clone(),
            line: r.line.clone(),
            way_ids: Vec::new(),
        });
        // The collapsed line's identity must not depend on member iteration
        // order: if a (malformed) master gathers members that disagree on
        // mode/ref/network, take the lexicographically smallest identity so the
        // merge is deterministic build-to-build. Uniform real masters (every
        // member shares one identity) are unaffected.
        let cur = (
            entry.mode.kind_str(),
            entry.line.as_str(),
            entry.network.as_str(),
        );
        let new = (r.mode.kind_str(), r.line.as_str(), r.network.as_str());
        if new < cur {
            entry.mode = r.mode;
            entry.line = r.line.clone();
            entry.network = r.network.clone();
        }
        entry.way_ids.extend(r.way_ids);
    }

    let mut features = Vec::new();
    for line in groups.into_values() {
        let Line {
            mode,
            network,
            line,
            way_ids,
        } = line;
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

        // Identity (ADR 0029). A line that joins a routable feed keeps its
        // feed/gtfsId-style id (`<prefix><gtfs route id>`) and `routable: true`;
        // an overlay-only line (geometry but no in-scope timetable) gets an
        // OSM-derived `OSM:<network>:<ref>` id and `routable: false`.
        let line_kind = mode.line_kind();
        let gkey = line_kind.map(|k| driver.gtfs_key(k, &line));
        let g = gkey.as_ref().and_then(|k| gtfs_routes.get(k));
        let (route, routable) = match (g, &gkey) {
            (Some(r), _) => (format!("{}{}", driver.route_id_prefix(), r.id), true),
            _ => (format!("OSM:{network}:{line}"), false),
        };
        // Metro keeps the driver colour; every other mode draws null (tram's
        // original behaviour, extended to the rail family).
        let color = match line_kind {
            Some(LineKind::Metro) => Some(
                g.and_then(|r| (!r.color.is_empty()).then(|| format!("#{}", r.color)))
                    .unwrap_or_else(|| driver.metro_color(&line).to_string()),
            ),
            _ => None,
        };

        features.push(json!({
            "type": "Feature",
            "properties": {
                "kind": mode.kind_str(), "route": route, "line": line,
                "color": color, "network": network, "routable": routable,
            },
            "geometry": { "type": "MultiLineString", "coordinates": members },
        }));
    }

    sort_metro_first(&mut features);
    Ok(json!({ "type": "FeatureCollection", "features": features }))
}

/// Pass 1: the driver-owned route relations in the widened set (ADR 0029,
/// `route=subway|tram|light_rail|rail|regional_rail`) with a ref, their track-way
/// members (members whose role doesn't start with `platform`), and the
/// `route_master` that groups each route's variants.
///
/// Two passes over the clip: first map each member route relation to its
/// `route_master`, then read the route relations and attach that master. The
/// network is the OSM `network` tag, falling back to `operator`. Malformed or
/// out-of-scope relations are skipped fail-soft (panic-free on odd OSM).
fn collect_route_relations(
    clip: &Path,
    driver: &dyn TransitOverlayDriver,
) -> anyhow::Result<Vec<RouteRel>> {
    // route relation id -> the route_master grouping it.
    let mut master_of: HashMap<i64, i64> = HashMap::new();
    ElementReader::from_path(clip)?.for_each(|el| {
        if let Element::Relation(rel) = el
            && rel.tags().any(|(k, v)| k == "type" && v == "route_master")
        {
            let master = rel.id();
            for m in rel.members() {
                if m.member_type == osmpbf::RelMemberType::Relation {
                    master_of.insert(m.member_id, master);
                }
            }
        }
    })?;

    let mut out = Vec::new();
    ElementReader::from_path(clip)?.for_each(|el| {
        if let Element::Relation(rel) = el {
            let mut route = None;
            let mut line = None;
            let mut operator = None;
            let mut network = None;
            for (k, v) in rel.tags() {
                match k {
                    "route" => route = Some(v.to_string()),
                    "ref" => line = Some(v.to_string()),
                    "operator" => operator = Some(v.to_string()),
                    "network" => network = Some(v.to_string()),
                    _ => {}
                }
            }
            let (Some(route), Some(line)) = (route, line) else {
                return;
            };
            // Scope to the driver's network (matched on either tag) — keeps a
            // region's overlay to its own operator while widening route modes.
            let owned = operator.as_deref() == Some(driver.operator())
                || network.as_deref() == Some(driver.operator());
            if !owned || line.is_empty() {
                return;
            }
            let Some(mode) = RouteMode::from_osm(&route, &line, driver) else {
                return;
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
                    mode,
                    network: network.or(operator).unwrap_or_default(),
                    line,
                    way_ids,
                    master: master_of.get(&rel.id()).copied(),
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

/// Metro first (A, B, C), then every other mode (tram + the rail family)
/// ascending by numeric ref, ties broken by the raw ref, then `kind` and
/// `network` as final tiebreakers so a multi-network or mixed-mode region with
/// two lines sharing a ref still sorts to a total order (deterministic bytes,
/// ADR 0029). Rome (single-network, unique refs) keeps its original order — its
/// keys never collide, so the trailing tiebreakers never fire.
fn sort_metro_first(features: &mut [Value]) {
    features.sort_by(|a, b| {
        let key = |f: &Value| {
            let p = &f["properties"];
            let kind = p["kind"].as_str().unwrap_or("").to_string();
            let line = p["line"].as_str().unwrap_or("").to_string();
            let network = p["network"].as_str().unwrap_or("").to_string();
            let is_not_metro = kind != "metro";
            let num = line.parse::<i64>().unwrap_or(i64::MAX);
            (is_not_metro, num, line, kind, network)
        };
        key(a).cmp(&key(b))
    });
}

fn round7(v: f64) -> f64 {
    (v * 1e7).round() / 1e7
}

// ─── metro-stations ──────────────────────────────────────────────────────────

// Metres per degree latitude — constant everywhere; the longitude scale and the
// projection origin are region-specific and come from the driver.
const M_LAT: f64 = 111_320.0;
const HALF_LEN_M: f64 = 52.0; // platform half-length along the track
const DENSIFY_M: f64 = 16.0;
const PLAT_GAP_M: f64 = 1.2; // offset from the track centreline to the near edge
const PLAT_WIDTH_M: f64 = 5.0;
const EXIT_ASSIGN_M: f64 = 400.0;
const HULL_CONCAVITY_M: f64 = 60.0;

// Concourse smoothing (ADR 0014's "morphological smoothing"): round the jagged
// hull into an organic footprint with pure, dependency-free geometry — Chaikin
// corner-cutting then Visvalingam-Whyatt simplification. Smoothing falls back to
// the raw hull for a station if it would invalidate the polygon, drop a stop, or
// distort the area beyond tolerance (see `smooth_ring`).
/// Chaikin corner-cutting passes applied to the hull ring.
const CHAIKIN_ITERS: usize = 2;
/// Visvalingam-Whyatt area tolerance (m²) trading vertex count for fidelity.
const SIMPLIFY_TOLERANCE_M2: f64 = 12.0;
/// Max fractional area change smoothing may introduce before we keep the raw hull.
const SMOOTH_AREA_TOLERANCE: f64 = 0.25;

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
    // station slug → just the stop points, which smoothing must not drop
    let mut stop_pts: HashMap<String, Vec<(f64, f64)>> = HashMap::new();
    // station slug → the platform strip rings (metres) that overlap the
    // concourse; these dissolve into the concourse footprint (ADR 0031).
    let mut plat_rings: HashMap<String, Vec<Vec<(f64, f64)>>> = HashMap::new();

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
            stop_pts
                .entry(slug.clone())
                .or_default()
                .push((stop.x, stop.y));

            if let Some(ring) = platform_ring(&tracks, (stop.x, stop.y)) {
                hull_pts
                    .entry(slug.clone())
                    .or_default()
                    .extend(ring.iter().copied());
                plat_rings
                    .entry(slug.clone())
                    .or_default()
                    .push(ring.clone());
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
        let stops = stop_pts.get(slug).map(Vec::as_slice).unwrap_or(&[]);
        let Some(hull) = station_hull_m(&hull_pts[slug], stops) else {
            continue;
        };
        // Corridor union (ADR 0031): dissolve the concourse hull with this
        // station's platform strips so an overhanging strip merges into one
        // footprint instead of emitting overlapping pieces. A station whose
        // strips already sit inside the hull is byte-unchanged.
        let plats = plat_rings.get(slug).map(Vec::as_slice).unwrap_or(&[]);
        let footprint = dissolve_footprint(&hull, plats, stops);
        let ring: Vec<Value> = footprint
            .iter()
            .map(|(x, y)| Value::Array(from_m(proj, *x, *y).iter().map(|v| json!(v)).collect()))
            .collect();
        features.push(json!({
            "type": "Feature",
            "properties": { "kind": "concourse", "station": slug, "level": 0 },
            "geometry": { "type": "Polygon", "coordinates": [ring] },
        }));
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

/// The concourse footprint of a station's points as a closed metre ring: a
/// concave hull (convex-hull fallback for sparse stations) smoothed into an
/// organic shape. `stop_pts` are the platform stop points the footprint must
/// still contain after smoothing.
fn station_hull_m(pts: &[(f64, f64)], stop_pts: &[(f64, f64)]) -> Option<Vec<(f64, f64)>> {
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
    Some(smooth_ring(&ring, stop_pts))
}

/// The concourse footprint as a lon/lat ring (projected `station_hull_m`).
/// Retained for the geometry tests; the build path projects `station_hull_m`
/// after the corridor dissolve.
#[cfg(test)]
fn station_hull(
    proj: Projection,
    pts: &[(f64, f64)],
    stop_pts: &[(f64, f64)],
) -> Option<Vec<Value>> {
    let ring = station_hull_m(pts, stop_pts)?;
    Some(
        ring.iter()
            .map(|(x, y)| Value::Array(from_m(proj, *x, *y).iter().map(|v| json!(v)).collect()))
            .collect(),
    )
}

/// Corridor union (ADR 0031): dissolve a station's concourse `hull` ring with
/// its overlapping platform/corridor strip `rings` into ONE clean footprint,
/// instead of emitting separate overlapping polygons. All inputs are closed
/// metre rings; the result is a closed metre ring.
///
/// Fail-soft and regression-safe:
/// - With no strips, or when every strip already sits inside the hull (or sits
///   *fully* outside it, touching nothing), the hull is returned
///   **byte-unchanged** — a single-polygon / non-overlapping station never
///   re-traces through the boolean op. Only a strip that genuinely straddles the
///   hull edge (a vertex outside *and* a vertex inside/on it) is dissolved.
/// - The union runs through `geo::unary_union` (i_overlay-backed). Of the output
///   polygons we keep the one whose interior covers the hull centroid — the
///   component that actually contains the station — not merely the largest by
///   area, so a far escaping strip can't steal the selection. If none qualifies,
///   or the dissolved ring is degenerate, not closed, invalid, or drops a stop
///   point, the raw hull is kept. Non-finite coordinates short-circuit too.
///   Any interior holes are dropped (exterior-only): a display footprint is a
///   solid silhouette, and the stop-coverage guard still forces fallback if a
///   stop would land in a hole.
fn dissolve_footprint(
    hull: &[(f64, f64)],
    rings: &[Vec<(f64, f64)>],
    stop_pts: &[(f64, f64)],
) -> Vec<(f64, f64)> {
    if hull.len() < 4 {
        return hull.to_vec();
    }
    let hull_poly = Polygon::new(LineString::from(hull.to_vec()), vec![]);
    if !hull_poly.is_valid() {
        return hull.to_vec();
    }

    // Only strips that genuinely straddle the hull edge need dissolving — a
    // vertex outside AND a vertex inside/on the ring. A strip already enclosed
    // adds nothing; a strip that's *fully* disjoint (every vertex outside,
    // touching nothing) would only re-trace the hull through the boolean op and
    // could steal the area selection, so both stay on the byte-unchanged path.
    let escaping: Vec<&Vec<(f64, f64)>> = rings
        .iter()
        .filter(|ring| {
            ring.len() >= 4
                && ring.iter().all(|(x, y)| x.is_finite() && y.is_finite())
                && ring.iter().any(|(x, y)| {
                    hull_poly.coordinate_position(&coord! { x: *x, y: *y }) == CoordPos::Outside
                })
                && ring.iter().any(|(x, y)| {
                    hull_poly.coordinate_position(&coord! { x: *x, y: *y }) != CoordPos::Outside
                })
        })
        .collect();
    if escaping.is_empty() {
        return hull.to_vec();
    }

    let mut polys = vec![hull_poly.clone()];
    for ring in escaping {
        let p = Polygon::new(LineString::from(ring.to_vec()), vec![]);
        if p.is_valid() {
            polys.push(p);
        }
    }
    if polys.len() < 2 {
        return hull.to_vec();
    }

    let union = unary_union(polys.iter());
    // Pick the component that actually contains the station: the one whose
    // interior covers the hull centroid. A straddling strip merges with the hull
    // into a single connected component, but guarding by containment (not area)
    // means a stray disjoint component can never be chosen as the footprint.
    let hull_centroid = hull_poly.centroid().map(|c| coord! { x: c.x(), y: c.y() });
    let Some(best) = union
        .0
        .iter()
        .find(|p| hull_centroid.is_some_and(|c| p.coordinate_position(&c) != CoordPos::Outside))
    else {
        return hull.to_vec();
    };
    // Exterior only: holes are intentionally filled — a display footprint is a
    // solid silhouette, and the stop guard below catches a stop inside a hole.
    let out: Vec<(f64, f64)> = best.exterior().points().map(|p| (p.x(), p.y())).collect();

    // Guard the dissolved ring the same way smoothing is guarded: it must be a
    // valid, closed, simple polygon that still covers every stop point.
    if out.len() < 4 || out.first() != out.last() || !best.is_valid() {
        return hull.to_vec();
    }
    if stop_pts
        .iter()
        .any(|(x, y)| best.coordinate_position(&coord! { x: *x, y: *y }) == CoordPos::Outside)
    {
        return hull.to_vec();
    }
    out
}

/// Round a closed hull ring into an organic concourse footprint: Chaikin
/// corner-cutting then Visvalingam-Whyatt simplification. Falls back to `ring`
/// unchanged if the smoothed result would be degenerate, self-intersecting, drop
/// a stop point, or distort the area beyond `SMOOTH_AREA_TOLERANCE`.
fn smooth_ring(ring: &[(f64, f64)], stop_pts: &[(f64, f64)]) -> Vec<(f64, f64)> {
    // Degenerate ring (fewer than a triangle's worth of closed vertices): nothing
    // to round, hand it back untouched.
    if ring.len() < 4 {
        return ring.to_vec();
    }
    let mut cut = ring.to_vec();
    for _ in 0..CHAIKIN_ITERS {
        cut = chaikin(&cut);
    }
    let line = LineString::from(cut);
    let simplified = Polygon::new(line, vec![]).simplify_vw_preserve(SIMPLIFY_TOLERANCE_M2);

    let exterior = simplified.exterior();
    let out: Vec<(f64, f64)> = exterior.points().map(|p| (p.x(), p.y())).collect();
    if out.len() < 4 || out.first() != out.last() || !simplified.is_valid() {
        return ring.to_vec();
    }

    // Smoothing must not shrink the footprint off its platforms, nor wildly
    // distort the hull area. A stop sitting exactly on the smoothed boundary
    // still counts as covered — many stops are hull vertices, and corner-cutting
    // lands them on the edge rather than strictly inside.
    if stop_pts
        .iter()
        .any(|(x, y)| simplified.coordinate_position(&coord! { x: *x, y: *y }) == CoordPos::Outside)
    {
        return ring.to_vec();
    }
    let raw_area = Polygon::new(LineString::from(ring.to_vec()), vec![])
        .unsigned_area()
        .max(1e-6);
    let delta = (simplified.unsigned_area() - raw_area).abs() / raw_area;
    // Fail soft on non-finite coords (NaN/inf from upstream): a NaN delta would
    // slip past `delta > tolerance`, so keep the raw hull instead.
    if !delta.is_finite() || delta > SMOOTH_AREA_TOLERANCE {
        return ring.to_vec();
    }
    out
}

/// One Chaikin corner-cutting pass over a closed ring (first == last): each edge
/// contributes its 1/4 and 3/4 points, rounding every corner. The result stays
/// closed.
fn chaikin(ring: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let n = ring.len();
    if n < 4 {
        return ring.to_vec();
    }
    // Operate on the open vertex sequence (drop the duplicate closing point).
    let open = &ring[..n - 1];
    let mut out = Vec::with_capacity(open.len() * 2 + 1);
    for i in 0..open.len() {
        let a = open[i];
        let b = open[(i + 1) % open.len()];
        out.push((a.0 * 0.75 + b.0 * 0.25, a.1 * 0.75 + b.1 * 0.25));
        out.push((a.0 * 0.25 + b.0 * 0.75, a.1 * 0.25 + b.1 * 0.75));
    }
    if let Some(first) = out.first().copied() {
        out.push(first);
    }
    out
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
    use geo::Contains;

    /// The local-planar projection the generic geometry tests run in (Rome's, via
    /// the driver) — the metre math is generic; we just need a concrete origin.
    fn test_proj() -> Projection {
        overlay_driver("italy", "rome").unwrap().projection()
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
        let ring = station_hull(test_proj(), &pts, &pts).unwrap();
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

    /// Build a polygon from a closed metre ring for the geometric assertions.
    fn poly(ring: &[(f64, f64)]) -> Polygon<f64> {
        Polygon::new(LineString::from(ring.to_vec()), vec![])
    }

    #[test]
    fn chaikin_rounds_a_square_corners() {
        // a closed unit-ish square (first == last)
        let square = vec![
            (0.0, 0.0),
            (100.0, 0.0),
            (100.0, 100.0),
            (0.0, 100.0),
            (0.0, 0.0),
        ];
        let cut = chaikin(&square);
        // one pass turns each of the 4 corners into 2 vertices → 8 (+ closing).
        assert_eq!(cut.len(), 9, "8 cut vertices + closing point");
        assert_eq!(cut.first(), cut.last(), "stays closed");
        // the (0,0) corner is specifically rounded: the first edge (0,0)->(100,0)
        // contributes its 1/4 and 3/4 blends, so the two vertices flanking the old
        // corner sit at (25,0) and (75,0), not on (0,0) itself.
        let near =
            |a: (f64, f64), b: (f64, f64)| (a.0 - b.0).abs() < 1e-9 && (a.1 - b.1).abs() < 1e-9;
        assert!(near(cut[0], (25.0, 0.0)), "1/4 blend along the first edge");
        assert!(near(cut[1], (75.0, 0.0)), "3/4 blend along the first edge");
        assert!(
            !cut.iter().any(|&v| near(v, (0.0, 0.0))),
            "the original corner is cut away"
        );
        // cut vertices stay inside the convex square (corner-cutting shrinks).
        assert!(
            cut.iter()
                .all(|(x, y)| (0.0..=100.0).contains(x) && (0.0..=100.0).contains(y))
        );
    }

    #[test]
    fn smoothing_keeps_a_valid_closed_polygon_containing_stops() {
        // a jagged-ish octagon-ish ring around stop points it must keep covering.
        let stops = vec![(20.0, 20.0), (80.0, 20.0), (50.0, 80.0)];
        let ring = vec![
            (0.0, 0.0),
            (50.0, -10.0),
            (100.0, 0.0),
            (110.0, 50.0),
            (100.0, 100.0),
            (50.0, 110.0),
            (0.0, 100.0),
            (-10.0, 50.0),
            (0.0, 0.0),
        ];
        let out = smooth_ring(&ring, &stops);
        assert!(out.len() >= 4, "non-degenerate ring");
        assert_eq!(out.first(), out.last(), "closed");
        let p = poly(&out);
        assert!(p.is_valid(), "no self-intersection");
        for (x, y) in &stops {
            assert!(p.contains(&GeoPoint::new(*x, *y)), "still covers the stop");
        }
        // area stays within the documented tolerance of the raw hull.
        let raw = poly(&ring).unsigned_area();
        let delta = (p.unsigned_area() - raw).abs() / raw;
        assert!(delta <= SMOOTH_AREA_TOLERANCE, "area within tolerance");
    }

    #[test]
    fn smoothing_does_not_blow_up_vertex_count() {
        // Chaikin densifies, but VW simplification must claw the count back so the
        // smoothed ring is not dramatically heavier than the Chaikin intermediate.
        let mut ring: Vec<(f64, f64)> = (0..12)
            .map(|i| {
                let a = std::f64::consts::TAU * i as f64 / 12.0;
                (50.0 + 60.0 * a.cos(), 50.0 + 60.0 * a.sin())
            })
            .collect();
        ring.push(ring[0]); // close the ring
        // Reproduce the pre-simplify Chaikin expansion the smoother runs.
        let mut expanded = ring.clone();
        for _ in 0..CHAIKIN_ITERS {
            expanded = chaikin(&expanded);
        }
        let out = smooth_ring(&ring, &[(50.0, 50.0)]);
        // The smoothing path actually ran (no fallback to the raw ring)...
        assert_ne!(out, ring, "smoothing applied rather than falling back");
        assert_eq!(out.first(), out.last(), "closed");
        // ...and VW brought the vertex count back below the Chaikin intermediate.
        assert!(
            out.len() < expanded.len(),
            "VW simplifies below the Chaikin expansion: {} vs expanded {}",
            out.len(),
            expanded.len()
        );
    }

    #[test]
    fn degenerate_inputs_fall_back_unchanged() {
        // <4 points: returned as-is (smoothing is a no-op).
        let tri = vec![(0.0, 0.0), (10.0, 0.0), (5.0, 10.0)];
        assert_eq!(smooth_ring(&tri, &[]), tri);
        // chaikin on a sub-4-vertex ring is identity.
        assert_eq!(chaikin(&tri), tri);
        // station_hull rejects <3 input points (degenerate, no concourse).
        assert!(station_hull(test_proj(), &[(0.0, 0.0), (1.0, 1.0)], &[]).is_none());
    }

    #[test]
    fn smoothing_falls_back_when_it_would_drop_a_stop() {
        // a stop sitting right on the hull boundary corner: Chaikin cuts that
        // corner inward, so the corner stop ends up outside → fall back to raw.
        let ring = vec![
            (0.0, 0.0),
            (100.0, 0.0),
            (100.0, 100.0),
            (0.0, 100.0),
            (0.0, 0.0),
        ];
        let corner_stop = vec![(0.0, 0.0)];
        let out = smooth_ring(&ring, &corner_stop);
        assert_eq!(
            out, ring,
            "kept the raw hull to keep covering the corner stop"
        );
    }

    #[test]
    fn smoothing_applies_when_a_stop_lies_on_the_smoothed_boundary() {
        // The old strict-interior guard fell back whenever a stop sat exactly on
        // the smoothed edge. Take a vertex of the smoothed ring as a stop: it lies
        // ON the boundary, so the relaxed guard must keep smoothing instead of
        // reverting to the raw hull.
        let ring = vec![
            (0.0, 0.0),
            (100.0, 0.0),
            (100.0, 100.0),
            (0.0, 100.0),
            (0.0, 0.0),
        ];
        let smoothed = smooth_ring(&ring, &[]);
        assert_ne!(smoothed, ring, "baseline: smoothing runs with no stops");
        // A vertex of the smoothed boundary, used as a stop point.
        let boundary_stop = smoothed[1];
        assert_eq!(
            poly(&smoothed).coordinate_position(&coord! { x: boundary_stop.0, y: boundary_stop.1 }),
            CoordPos::OnBoundary,
            "the chosen stop is exactly on the smoothed edge"
        );
        let out = smooth_ring(&ring, &[boundary_stop]);
        assert_eq!(
            out, smoothed,
            "on-boundary stop still counts as covered → smoothing applies"
        );
    }

    #[test]
    fn smoothing_falls_back_on_self_intersecting_input() {
        // A bowtie (figure-eight) ring is self-intersecting; whatever Chaikin/VW
        // produce, the validity guard must reject it and hand back the raw ring.
        let bowtie = vec![
            (0.0, 0.0),
            (100.0, 100.0),
            (0.0, 100.0),
            (100.0, 0.0),
            (0.0, 0.0),
        ];
        let out = smooth_ring(&bowtie, &[]);
        assert_eq!(out, bowtie, "invalid smoothed polygon falls back to raw");
    }

    #[test]
    fn smoothing_falls_back_on_excessive_area_distortion() {
        // A star-like ring with deep spikes: Chaikin corner-cutting collapses the
        // spikes, shrinking the area well past SMOOTH_AREA_TOLERANCE, so the guard
        // keeps the raw hull. No stops, so only the area branch can fire.
        let mut ring = Vec::new();
        let spikes = 8;
        for i in 0..spikes {
            let outer = std::f64::consts::TAU * i as f64 / spikes as f64;
            let inner = std::f64::consts::TAU * (i as f64 + 0.5) / spikes as f64;
            ring.push((100.0 * outer.cos(), 100.0 * outer.sin()));
            ring.push((4.0 * inner.cos(), 4.0 * inner.sin()));
        }
        ring.push(ring[0]); // close
        let raw_area = poly(&ring).unsigned_area();
        let out = smooth_ring(&ring, &[]);
        assert_eq!(
            out, ring,
            "area distortion beyond tolerance falls back to the raw star"
        );
        // Sanity: the distortion really did exceed the tolerance.
        let mut cut = ring.clone();
        for _ in 0..CHAIKIN_ITERS {
            cut = chaikin(&cut);
        }
        let smoothed =
            Polygon::new(LineString::from(cut), vec![]).simplify_vw_preserve(SIMPLIFY_TOLERANCE_M2);
        // The smoothed star stays valid, so the area branch — not the validity
        // guard — is what forces the fallback.
        assert!(smoothed.is_valid(), "smoothed star is a valid polygon");
        let smoothed_area = smoothed.unsigned_area();
        assert!(
            (smoothed_area - raw_area).abs() / raw_area > SMOOTH_AREA_TOLERANCE,
            "the engineered input distorts area past tolerance"
        );
    }

    #[test]
    fn station_hull_emits_a_smoothed_ring_covering_stops_in_wgs84() {
        // End-to-end: project -> hull -> smooth -> unproject. The returned lon/lat
        // ring must be closed, valid, and still cover every stop after the round
        // trip back through from_m, where float error could nudge a boundary stop.
        let proj = test_proj();
        let pts = vec![
            (0.0, 0.0),
            (120.0, 0.0),
            (120.0, 80.0),
            (0.0, 80.0),
            (60.0, 40.0),
        ];
        let stops = vec![(20.0, 20.0), (100.0, 20.0), (60.0, 60.0)];
        let ring = station_hull(proj, &pts, &stops).unwrap();
        // Reproject the WGS84 ring back into metres.
        let metres: Vec<(f64, f64)> = ring
            .iter()
            .map(|c| {
                let lon = c[0].as_f64().unwrap();
                let lat = c[1].as_f64().unwrap();
                to_m(proj, lon, lat)
            })
            .collect();
        assert!(metres.len() >= 4, "non-degenerate ring");
        assert_eq!(metres.first(), metres.last(), "closed after round trip");
        let p = poly(&metres);
        assert!(p.is_valid(), "valid after round trip");
        for (x, y) in &stops {
            // Allow on-boundary; round7 plus projection can land a stop on the edge.
            assert_ne!(
                p.coordinate_position(&coord! { x: *x, y: *y }),
                CoordPos::Outside,
                "stop still covered after the WGS84 round trip"
            );
        }
    }

    // ── corridor union / footprint dissolve (ADR 0031) ───────────────────────

    /// A platform strip that pokes out of the concourse hull dissolves into one
    /// merged footprint: the dissolved ring grows to cover the overhang, stays a
    /// valid closed polygon, and contains both the hull and the strip area.
    #[test]
    fn dissolve_merges_an_overhanging_strip_into_one_footprint() {
        // A 100×60 concourse hull and a strip sticking out past its top edge.
        let hull = vec![
            (0.0, 0.0),
            (100.0, 0.0),
            (100.0, 60.0),
            (0.0, 60.0),
            (0.0, 0.0),
        ];
        let strip = vec![
            (40.0, 50.0),
            (60.0, 50.0),
            (60.0, 90.0), // 30 m above the hull's top edge
            (40.0, 90.0),
            (40.0, 50.0),
        ];
        let out = dissolve_footprint(&hull, std::slice::from_ref(&strip), &[]);
        assert_ne!(out, hull, "the overhang forced a real dissolve");
        let p = poly(&out);
        assert!(p.is_valid(), "dissolved footprint is a valid polygon");
        assert_eq!(out.first(), out.last(), "closed ring");
        // One footprint that covers the hull interior AND the strip overhang.
        assert_ne!(
            p.coordinate_position(&coord! { x: 50.0, y: 30.0 }),
            CoordPos::Outside,
            "covers the hull interior"
        );
        assert_ne!(
            p.coordinate_position(&coord! { x: 50.0, y: 80.0 }),
            CoordPos::Outside,
            "covers the strip overhang the raw hull missed"
        );
        // The raw hull did NOT cover the overhang — proving the union added area.
        assert_eq!(
            poly(&hull).coordinate_position(&coord! { x: 50.0, y: 80.0 }),
            CoordPos::Outside,
        );
    }

    /// A strip already contained in the hull is a no-op: the footprint is the raw
    /// hull, byte-for-byte (a non-overlapping / contained station never re-traces
    /// through the boolean op, so the shipped output is unchanged).
    #[test]
    fn dissolve_is_byte_unchanged_when_strip_is_inside_hull() {
        let hull = vec![
            (0.0, 0.0),
            (100.0, 0.0),
            (100.0, 100.0),
            (0.0, 100.0),
            (0.0, 0.0),
        ];
        let inside = vec![
            (40.0, 40.0),
            (60.0, 40.0),
            (60.0, 60.0),
            (40.0, 60.0),
            (40.0, 40.0),
        ];
        let out = dissolve_footprint(&hull, &[inside], &[(50.0, 50.0)]);
        assert_eq!(out, hull, "contained strip leaves the hull byte-identical");
    }

    /// No strips at all (a sparse station): the footprint is the raw hull,
    /// byte-for-byte.
    #[test]
    fn dissolve_with_no_strips_is_byte_unchanged() {
        let hull = vec![
            (0.0, 0.0),
            (50.0, -10.0),
            (100.0, 0.0),
            (100.0, 100.0),
            (0.0, 100.0),
            (0.0, 0.0),
        ];
        assert_eq!(dissolve_footprint(&hull, &[], &[]), hull);
    }

    /// The dissolved footprint still contains the station's stop points, and a
    /// degenerate hull (sub-quad ring) falls back unchanged.
    #[test]
    fn dissolve_preserves_stops_and_falls_back_on_degenerate_hull() {
        let hull = vec![
            (0.0, 0.0),
            (100.0, 0.0),
            (100.0, 60.0),
            (0.0, 60.0),
            (0.0, 0.0),
        ];
        let strip = vec![
            (40.0, 40.0),
            (60.0, 40.0),
            (60.0, 100.0),
            (40.0, 100.0),
            (40.0, 40.0),
        ];
        let stops = vec![(10.0, 10.0), (90.0, 50.0), (50.0, 80.0)];
        let out = dissolve_footprint(&hull, &[strip], &stops);
        let p = poly(&out);
        for (x, y) in &stops {
            assert_ne!(
                p.coordinate_position(&coord! { x: *x, y: *y }),
                CoordPos::Outside,
                "every stop stays covered after the dissolve"
            );
        }
        // Degenerate hull: a triangle ring (<4 closed vertices) is handed back.
        let tri = vec![(0.0, 0.0), (10.0, 0.0), (5.0, 10.0)];
        assert_eq!(dissolve_footprint(&tri, &[], &[]), tri);
    }

    /// Fail-soft on malformed strip geometry: non-finite or sub-quad strip rings
    /// are ignored, so a NaN-laced strip can never panic or corrupt the footprint
    /// — the station falls back to its raw hull.
    #[test]
    fn dissolve_ignores_malformed_strips() {
        let hull = vec![
            (0.0, 0.0),
            (100.0, 0.0),
            (100.0, 100.0),
            (0.0, 100.0),
            (0.0, 0.0),
        ];
        let nan_strip = vec![
            (40.0, 90.0),
            (f64::NAN, 90.0),
            (60.0, 120.0),
            (40.0, 120.0),
            (40.0, 90.0),
        ];
        let tiny_strip = vec![(50.0, 95.0), (51.0, 96.0)]; // sub-quad
        let out = dissolve_footprint(&hull, &[nan_strip, tiny_strip], &[]);
        assert_eq!(out, hull, "malformed strips are skipped, hull kept");
    }

    /// A strip sitting *entirely* outside the hull, touching nothing, must not be
    /// dissolved: it would only re-trace the hull through the boolean op (breaking
    /// byte stability) or steal the selection. The hull is returned byte-for-byte.
    #[test]
    fn dissolve_is_byte_unchanged_when_strip_is_fully_disjoint() {
        let hull = vec![
            (0.0, 0.0),
            (100.0, 0.0),
            (100.0, 100.0),
            (0.0, 100.0),
            (0.0, 0.0),
        ];
        // A strip far away that overlaps the hull nowhere.
        let far = vec![
            (200.0, 200.0),
            (250.0, 200.0),
            (250.0, 260.0),
            (200.0, 260.0),
            (200.0, 200.0),
        ];
        let out = dissolve_footprint(&hull, std::slice::from_ref(&far), &[(50.0, 50.0)]);
        assert_eq!(out, hull, "a disjoint strip never enters the union");
    }

    /// When a genuine overhang and a far disjoint strip both "escape", the union
    /// must keep the component containing the station (covering the overhang), not
    /// the larger far strip. Guards the med finding: a bigger disjoint piece can't
    /// steal the max-area selection and discard the real overhang.
    #[test]
    fn dissolve_keeps_station_component_not_the_larger_disjoint_strip() {
        let hull = vec![
            (0.0, 0.0),
            (40.0, 0.0),
            (40.0, 40.0),
            (0.0, 40.0),
            (0.0, 0.0),
        ];
        // A small strip straddling the top edge — the real overhang to merge.
        let overhang = vec![
            (10.0, 30.0),
            (30.0, 30.0),
            (30.0, 60.0),
            (10.0, 60.0),
            (10.0, 30.0),
        ];
        // A far disjoint 100×100 strip — area 10000, far larger than the merged
        // body, and straddling its own nothing (it is fully outside the hull, so
        // the predicate already excludes it; included here to prove robustness).
        let big = vec![
            (200.0, 200.0),
            (300.0, 200.0),
            (300.0, 300.0),
            (200.0, 300.0),
            (200.0, 200.0),
        ];
        let stops = vec![(20.0, 20.0)];
        let out = dissolve_footprint(&hull, &[overhang, big], &stops);
        let p = poly(&out);
        // The footprint covers the hull centre and the overhang the hull missed.
        assert_ne!(
            p.coordinate_position(&coord! { x: 20.0, y: 20.0 }),
            CoordPos::Outside,
            "footprint contains the station, not the far strip"
        );
        assert_ne!(
            p.coordinate_position(&coord! { x: 20.0, y: 50.0 }),
            CoordPos::Outside,
            "the genuine overhang merged in"
        );
        // The far disjoint strip's centre is NOT the emitted footprint.
        assert_eq!(
            p.coordinate_position(&coord! { x: 250.0, y: 250.0 }),
            CoordPos::Outside,
            "the far strip was never selected as the concourse"
        );
    }

    /// The production build path stays byte-identical to the pre-union hull when a
    /// station's strips are enclosed: `dissolve_footprint(station_hull_m(..), ..)`
    /// equals `station_hull_m(..)`. This pins ADR 0031's byte-stability invariant
    /// (the load-bearing path for the Rome golden) as a real regression guard, not
    /// just prose, at the exact composition `build_metro_stations` uses.
    #[test]
    fn build_path_is_byte_identical_when_strips_are_enclosed() {
        // A dense cluster of points whose smoothed concave hull encloses a small
        // central strip — the common, non-overhanging station configuration.
        let pts = vec![
            (0.0, 0.0),
            (100.0, 0.0),
            (100.0, 100.0),
            (0.0, 100.0),
            (50.0, -5.0),
            (105.0, 50.0),
            (50.0, 105.0),
            (-5.0, 50.0),
        ];
        let stops = vec![(50.0, 50.0)];
        let hull = station_hull_m(&pts, &stops).expect("hull builds");
        // A strip well inside that smoothed hull, so nothing escapes.
        let enclosed = vec![
            (45.0, 45.0),
            (55.0, 45.0),
            (55.0, 55.0),
            (45.0, 55.0),
            (45.0, 45.0),
        ];
        let footprint = dissolve_footprint(&hull, std::slice::from_ref(&enclosed), &stops);
        assert_eq!(
            footprint, hull,
            "an enclosed strip leaves the projected concourse ring byte-for-byte"
        );
    }

    /// End-to-end through `build_metro_stations`: a Rome (ATAC) two-stop line
    /// emits exactly one concourse per station, with the platforms still present
    /// as their own features — the dissolve never drops or duplicates a station.
    #[test]
    fn metro_stations_emit_one_concourse_per_station() {
        let dir = tempfile::tempdir().unwrap();
        let clip = dir.path().join("clip.osm.pbf");

        let mut osm = OsmBuilder::new();
        // Two stop nodes (named stations) plus a subway track way under them.
        let s1 = osm.node(41.900, 12.490, &[("name", "Alpha")]);
        let s2 = osm.node(41.905, 12.495, &[("name", "Beta")]);
        // A track way passing through both stops so platform strips can form.
        let t0 = osm.node(41.899, 12.489, &[]);
        let t3 = osm.node(41.906, 12.496, &[]);
        let track = osm.way_tagged(&[t0, s1, s2, t3], &[("railway", "subway")]);
        let _ = track;
        osm.relation(
            &[("route", "subway"), ("ref", "A"), ("operator", "ATAC")],
            &[(Member::Node, s1, "stop"), (Member::Node, s2, "stop")],
        );
        osm.write(&clip);

        let fc = build_metro_stations(&clip, rome_driver().as_ref()).unwrap();
        let feats = fc["features"].as_array().unwrap();
        let concourses: Vec<&Value> = feats
            .iter()
            .filter(|f| f["properties"]["kind"] == json!("concourse"))
            .collect();
        // Exactly one concourse per named station — no duplicates, none dropped.
        let stations: HashSet<&str> = concourses
            .iter()
            .map(|f| f["properties"]["station"].as_str().unwrap())
            .collect();
        assert_eq!(stations, HashSet::from(["alpha", "beta"]));
        assert_eq!(concourses.len(), 2, "one footprint per station");
        // Each concourse polygon is closed and non-degenerate.
        for c in &concourses {
            let ring = c["geometry"]["coordinates"][0].as_array().unwrap();
            assert!(ring.len() >= 4, "closed concourse ring");
            assert_eq!(ring.first(), ring.last(), "ring closes");
        }
        // Platforms are still emitted as their own features (additive layers).
        assert!(
            feats
                .iter()
                .any(|f| f["properties"]["kind"] == json!("platform")),
            "platform strips remain as separate features"
        );
    }

    // ── transit-lines (multi-region generalization, ADR 0029) ────────────────

    /// The driver every transit-lines build test runs through (Rome/ATAC).
    fn rome_driver() -> std::sync::Arc<dyn TransitOverlayDriver> {
        overlay_driver("italy", "rome").unwrap()
    }

    /// A GTFS zip with `routes.txt` rows `(route_id, short_name, type, color)`.
    fn write_gtfs(path: &Path, rows: &[(&str, &str, &str, &str)]) {
        use std::io::Write;
        let f = std::fs::File::create(path).unwrap();
        let mut z = zip::ZipWriter::new(f);
        z.start_file::<_, ()>("routes.txt", zip::write::SimpleFileOptions::default())
            .unwrap();
        let mut text = String::from("route_id,route_short_name,route_type,route_color\n");
        for (id, short, ty, color) in rows {
            text.push_str(&format!("{id},{short},{ty},{color}\n"));
        }
        z.write_all(text.as_bytes()).unwrap();
        z.finish().unwrap();
    }

    /// Look up a feature by its `line` ref.
    fn feature_for<'a>(fc: &'a Value, line: &str) -> Option<&'a Value> {
        fc["features"]
            .as_array()?
            .iter()
            .find(|f| f["properties"]["line"] == json!(line))
    }

    /// The ordered list of `line` refs in the FeatureCollection.
    fn line_order(fc: &Value) -> Vec<String> {
        fc["features"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| f["properties"]["line"].as_str().unwrap().to_string())
            .collect()
    }

    /// A `route=subway` metro line (A/B/C) builds with the GTFS id + colour and
    /// the additive `network`/`routable` identity props — locking the Rome shape.
    #[test]
    fn metro_line_keeps_gtfs_id_colour_and_is_routable() {
        let dir = tempfile::tempdir().unwrap();
        let clip = dir.path().join("clip.osm.pbf");
        let gtfs = dir.path().join("g.zip");
        write_gtfs(&gtfs, &[("MEA", "MEA", "1", "E27439")]);

        let mut osm = OsmBuilder::new();
        let n1 = osm.node(41.90, 12.49, &[]);
        let n2 = osm.node(41.91, 12.50, &[]);
        let n3 = osm.node(41.92, 12.51, &[]);
        let w1 = osm.way(&[n1, n2]);
        let w2 = osm.way(&[n2, n3]);
        osm.relation(
            &[("route", "subway"), ("ref", "A"), ("operator", "ATAC")],
            &[(Member::Way, w1, ""), (Member::Way, w2, "")],
        );
        osm.write(&clip);

        let fc = build_transit_lines(&clip, &gtfs, rome_driver().as_ref()).unwrap();
        let f = feature_for(&fc, "A").unwrap();
        assert_eq!(f["properties"]["kind"], json!("metro"));
        assert_eq!(f["properties"]["route"], json!("ATAC:MEA"));
        assert_eq!(f["properties"]["color"], json!("#E27439"));
        assert_eq!(f["properties"]["network"], json!("ATAC"));
        assert_eq!(f["properties"]["routable"], json!(true));
        // Two ways unioned into one MultiLineString.
        assert_eq!(
            f["geometry"]["coordinates"].as_array().unwrap().len(),
            2,
            "both track ways unioned into the line"
        );
    }

    /// A tram line: null colour, GTFS id, routable when present in the feed —
    /// matching the original tram behaviour exactly.
    #[test]
    fn tram_line_has_null_colour_and_gtfs_id() {
        let dir = tempfile::tempdir().unwrap();
        let clip = dir.path().join("clip.osm.pbf");
        let gtfs = dir.path().join("g.zip");
        write_gtfs(&gtfs, &[("R8", "8", "0", "")]);

        let mut osm = OsmBuilder::new();
        let a = osm.node(41.90, 12.49, &[]);
        let b = osm.node(41.91, 12.50, &[]);
        let w = osm.way(&[a, b]);
        osm.relation(
            &[("route", "tram"), ("ref", "8"), ("operator", "ATAC")],
            &[(Member::Way, w, "")],
        );
        osm.write(&clip);

        let fc = build_transit_lines(&clip, &gtfs, rome_driver().as_ref()).unwrap();
        let f = feature_for(&fc, "8").unwrap();
        assert_eq!(f["properties"]["kind"], json!("tram"));
        assert_eq!(f["properties"]["route"], json!("ATAC:R8"));
        assert_eq!(f["properties"]["color"], Value::Null);
        assert_eq!(f["properties"]["routable"], json!(true));
    }

    /// The widened route set (ADR 0029) now includes light_rail and
    /// regional_rail. With no in-scope feed row they draw as generic overlay-only
    /// lines: null colour, `routable: false`, and an `OSM:<network>:<ref>` id.
    #[test]
    fn light_rail_and_regional_rail_are_included_as_overlay_only() {
        let dir = tempfile::tempdir().unwrap();
        let clip = dir.path().join("clip.osm.pbf");
        let gtfs = dir.path().join("g.zip");
        write_gtfs(&gtfs, &[]); // empty feed — nothing routable

        let mut osm = OsmBuilder::new();
        let a = osm.node(41.90, 12.49, &[]);
        let b = osm.node(41.91, 12.50, &[]);
        let c = osm.node(41.92, 12.51, &[]);
        let lw = osm.way(&[a, b]);
        let rw = osm.way(&[b, c]);
        osm.relation(
            &[("route", "light_rail"), ("ref", "TVA"), ("network", "ATAC")],
            &[(Member::Way, lw, "")],
        );
        osm.relation(
            &[
                ("route", "regional_rail"),
                ("ref", "FL1"),
                ("operator", "ATAC"),
            ],
            &[(Member::Way, rw, "")],
        );
        osm.write(&clip);

        let fc = build_transit_lines(&clip, &gtfs, rome_driver().as_ref()).unwrap();
        let lr = feature_for(&fc, "TVA").unwrap();
        assert_eq!(lr["properties"]["kind"], json!("light_rail"));
        assert_eq!(lr["properties"]["route"], json!("OSM:ATAC:TVA"));
        assert_eq!(lr["properties"]["color"], Value::Null);
        assert_eq!(lr["properties"]["network"], json!("ATAC"));
        assert_eq!(lr["properties"]["routable"], json!(false));

        let rr = feature_for(&fc, "FL1").unwrap();
        assert_eq!(rr["properties"]["kind"], json!("regional_rail"));
        assert_eq!(rr["properties"]["route"], json!("OSM:ATAC:FL1"));
        assert_eq!(rr["properties"]["routable"], json!(false));
    }

    /// A `route_master` collapses its member route variants into a single line,
    /// unioning the ways from every variant (shared way deduped once).
    #[test]
    fn route_master_collapses_member_variants_into_one_line() {
        let dir = tempfile::tempdir().unwrap();
        let clip = dir.path().join("clip.osm.pbf");
        let gtfs = dir.path().join("g.zip");
        write_gtfs(&gtfs, &[("MEC", "MEC", "1", "008456")]);

        let mut osm = OsmBuilder::new();
        let n1 = osm.node(41.90, 12.49, &[]);
        let n2 = osm.node(41.91, 12.50, &[]);
        let n3 = osm.node(41.92, 12.51, &[]);
        let shared = osm.way(&[n1, n2]); // carried by both directions
        let extra = osm.way(&[n2, n3]); // only the return direction
        // Two direction route relations of line C.
        let fwd = osm.relation(
            &[("route", "subway"), ("ref", "C"), ("operator", "ATAC")],
            &[(Member::Way, shared, "")],
        );
        let bwd = osm.relation(
            &[("route", "subway"), ("ref", "C"), ("operator", "ATAC")],
            &[(Member::Way, shared, ""), (Member::Way, extra, "")],
        );
        osm.relation(
            &[("type", "route_master"), ("route_master", "subway")],
            &[(Member::Relation, fwd, ""), (Member::Relation, bwd, "")],
        );
        osm.write(&clip);

        let fc = build_transit_lines(&clip, &gtfs, rome_driver().as_ref()).unwrap();
        let cs: Vec<&Value> = fc["features"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["properties"]["line"] == json!("C"))
            .collect();
        assert_eq!(cs.len(), 1, "the two C variants collapse to one line");
        // Shared way deduped: exactly the 2 distinct ways, not 3.
        assert_eq!(
            cs[0]["geometry"]["coordinates"].as_array().unwrap().len(),
            2,
            "shared way counted once across both directions"
        );
    }

    /// Metro sorts before the rail family / trams, then numeric — the additive
    /// rail modes don't disturb the metro-first order.
    #[test]
    fn metro_first_order_holds_with_widened_modes() {
        let dir = tempfile::tempdir().unwrap();
        let clip = dir.path().join("clip.osm.pbf");
        let gtfs = dir.path().join("g.zip");
        write_gtfs(&gtfs, &[("MEB", "MEB", "1", "0570B5")]);

        let mut osm = OsmBuilder::new();
        let mk = |osm: &mut OsmBuilder, lon: f64| {
            let a = osm.node(41.90, lon, &[]);
            let b = osm.node(41.91, lon + 0.01, &[]);
            osm.way(&[a, b])
        };
        let wt = mk(&mut osm, 12.40);
        let wm = mk(&mut osm, 12.50);
        let wr = mk(&mut osm, 12.60);
        osm.relation(
            &[("route", "tram"), ("ref", "2"), ("operator", "ATAC")],
            &[(Member::Way, wt, "")],
        );
        osm.relation(
            &[("route", "subway"), ("ref", "B"), ("operator", "ATAC")],
            &[(Member::Way, wm, "")],
        );
        osm.relation(
            &[
                ("route", "regional_rail"),
                ("ref", "FL3"),
                ("operator", "ATAC"),
            ],
            &[(Member::Way, wr, "")],
        );
        osm.write(&clip);

        let fc = build_transit_lines(&clip, &gtfs, rome_driver().as_ref()).unwrap();
        // Metro B first; then the non-metro refs sorted numerically ("2" < "FL3").
        assert_eq!(line_order(&fc), ["B", "2", "FL3"]);
    }

    /// Fail-soft: a relation with no track ways, a foreign operator, and a
    /// route mode outside the set are all skipped without panicking, and an
    /// unresolvable-but-valid line still emits as a generic line.
    #[test]
    fn malformed_and_out_of_scope_relations_are_skipped_fail_soft() {
        let dir = tempfile::tempdir().unwrap();
        let clip = dir.path().join("clip.osm.pbf");
        let gtfs = dir.path().join("g.zip");
        write_gtfs(&gtfs, &[]);

        let mut osm = OsmBuilder::new();
        let a = osm.node(41.90, 12.49, &[]);
        let b = osm.node(41.91, 12.50, &[]);
        let w = osm.way(&[a, b]);
        // No way members at all (only a platform-role node) → skipped.
        osm.relation(
            &[("route", "subway"), ("ref", "A"), ("operator", "ATAC")],
            &[(Member::Node, a, "platform")],
        );
        // Foreign operator → skipped.
        osm.relation(
            &[("route", "tram"), ("ref", "99"), ("operator", "OTHER")],
            &[(Member::Way, w, "")],
        );
        // Out-of-set route mode (bus) → skipped.
        osm.relation(
            &[("route", "bus"), ("ref", "100"), ("operator", "ATAC")],
            &[(Member::Way, w, "")],
        );
        // A valid in-scope line with no feed row → emits as a generic line.
        osm.relation(
            &[("route", "tram"), ("ref", "19"), ("operator", "ATAC")],
            &[(Member::Way, w, "")],
        );
        osm.write(&clip);

        let fc = build_transit_lines(&clip, &gtfs, rome_driver().as_ref()).unwrap();
        let lines = line_order(&fc);
        assert_eq!(lines, ["19"], "only the valid in-scope line survives");
        let f = feature_for(&fc, "19").unwrap();
        assert_eq!(f["properties"]["route"], json!("OSM:ATAC:19"));
        assert_eq!(f["properties"]["routable"], json!(false));
    }

    /// A relation tagged `ref=B` but with empty `ref` is dropped, and a clip with
    /// junk bytes surfaces an error rather than panicking.
    #[test]
    fn empty_ref_dropped_and_junk_clip_errors() {
        let dir = tempfile::tempdir().unwrap();
        let clip = dir.path().join("clip.osm.pbf");
        let gtfs = dir.path().join("g.zip");
        write_gtfs(&gtfs, &[]);

        let mut osm = OsmBuilder::new();
        let a = osm.node(41.90, 12.49, &[]);
        let b = osm.node(41.91, 12.50, &[]);
        let w = osm.way(&[a, b]);
        osm.relation(
            &[("route", "tram"), ("ref", ""), ("operator", "ATAC")],
            &[(Member::Way, w, "")],
        );
        osm.write(&clip);
        let fc = build_transit_lines(&clip, &gtfs, rome_driver().as_ref()).unwrap();
        assert!(
            fc["features"].as_array().unwrap().is_empty(),
            "empty-ref line dropped"
        );

        let junk = dir.path().join("junk.osm.pbf");
        std::fs::write(&junk, b"not a pbf at all").unwrap();
        assert!(build_transit_lines(&junk, &gtfs, rome_driver().as_ref()).is_err());
    }

    /// `route=rail` maps to the `rail` mode and, with no in-scope feed row, draws
    /// as an overlay-only generic line (null colour, `routable:false`, OSM id) —
    /// the one widened mode the other tests don't exercise.
    #[test]
    fn route_rail_draws_as_overlay_only() {
        let dir = tempfile::tempdir().unwrap();
        let clip = dir.path().join("clip.osm.pbf");
        let gtfs = dir.path().join("g.zip");
        write_gtfs(&gtfs, &[]);

        let mut osm = OsmBuilder::new();
        let a = osm.node(41.90, 12.49, &[]);
        let b = osm.node(41.91, 12.50, &[]);
        let w = osm.way(&[a, b]);
        osm.relation(
            &[("route", "rail"), ("ref", "FR1"), ("operator", "ATAC")],
            &[(Member::Way, w, "")],
        );
        osm.write(&clip);

        let fc = build_transit_lines(&clip, &gtfs, rome_driver().as_ref()).unwrap();
        let f = feature_for(&fc, "FR1").unwrap();
        assert_eq!(f["properties"]["kind"], json!("rail"));
        assert_eq!(f["properties"]["route"], json!("OSM:ATAC:FR1"));
        assert_eq!(f["properties"]["color"], Value::Null);
        assert_eq!(f["properties"]["routable"], json!(false));
    }

    /// A `route=subway` relation whose ref is NOT a driver metro line hits the
    /// early `is_metro_line` gate in `RouteMode::from_osm` and is dropped — it
    /// never becomes a generic line.
    #[test]
    fn non_metro_subway_ref_is_dropped() {
        let dir = tempfile::tempdir().unwrap();
        let clip = dir.path().join("clip.osm.pbf");
        let gtfs = dir.path().join("g.zip");
        write_gtfs(&gtfs, &[]);

        let mut osm = OsmBuilder::new();
        let a = osm.node(41.90, 12.49, &[]);
        let b = osm.node(41.91, 12.50, &[]);
        let w = osm.way(&[a, b]);
        // "Z" is not an ATAC metro line, so the subway arm yields None.
        osm.relation(
            &[("route", "subway"), ("ref", "Z"), ("operator", "ATAC")],
            &[(Member::Way, w, "")],
        );
        osm.write(&clip);

        let fc = build_transit_lines(&clip, &gtfs, rome_driver().as_ref()).unwrap();
        assert!(
            fc["features"].as_array().unwrap().is_empty(),
            "a non-metro subway ref is gated out, not drawn as a generic line"
        );
    }

    /// Fallback `(network, mode, ref)` grouping with NO `route_master`: two same-
    /// ref same-operator variants collapse to one feature (ways deduped); two
    /// different refs stay as separate features.
    #[test]
    fn fallback_grouping_collapses_same_ref_keeps_distinct() {
        let dir = tempfile::tempdir().unwrap();
        let clip = dir.path().join("clip.osm.pbf");
        let gtfs = dir.path().join("g.zip");
        write_gtfs(&gtfs, &[]);

        let mut osm = OsmBuilder::new();
        let n1 = osm.node(41.90, 12.49, &[]);
        let n2 = osm.node(41.91, 12.50, &[]);
        let n3 = osm.node(41.92, 12.51, &[]);
        let shared = osm.way(&[n1, n2]); // both ref-8 variants carry this
        let extra = osm.way(&[n2, n3]); // only the second variant
        let other = osm.way(&[n1, n3]); // ref 14
        // Two ref-8 tram variants, no route_master → fallback collapse.
        osm.relation(
            &[("route", "tram"), ("ref", "8"), ("operator", "ATAC")],
            &[(Member::Way, shared, "")],
        );
        osm.relation(
            &[("route", "tram"), ("ref", "8"), ("operator", "ATAC")],
            &[(Member::Way, shared, ""), (Member::Way, extra, "")],
        );
        // A distinct ref stays separate.
        osm.relation(
            &[("route", "tram"), ("ref", "14"), ("operator", "ATAC")],
            &[(Member::Way, other, "")],
        );
        osm.write(&clip);

        let fc = build_transit_lines(&clip, &gtfs, rome_driver().as_ref()).unwrap();
        assert_eq!(line_order(&fc), ["8", "14"], "two lines, ref 8 collapsed");
        let eight = feature_for(&fc, "8").unwrap();
        assert_eq!(
            eight["geometry"]["coordinates"].as_array().unwrap().len(),
            2,
            "shared way deduped: 2 distinct ways across both variants"
        );
    }

    /// Fallback grouping keys on the network too: same ref + mode under two
    /// different networks stay as separate features (no cross-network merge).
    #[test]
    fn fallback_grouping_keeps_distinct_networks_separate() {
        let dir = tempfile::tempdir().unwrap();
        let clip = dir.path().join("clip.osm.pbf");
        let gtfs = dir.path().join("g.zip");
        write_gtfs(&gtfs, &[]);

        let mut osm = OsmBuilder::new();
        let n1 = osm.node(41.90, 12.49, &[]);
        let n2 = osm.node(41.91, 12.50, &[]);
        let n3 = osm.node(41.92, 12.51, &[]);
        let wa = osm.way(&[n1, n2]);
        let wb = osm.way(&[n2, n3]);
        // Same ref "5", same mode; the driver owns both because operator==ATAC,
        // but the recorded network differs (network tag preferred over operator).
        osm.relation(
            &[
                ("route", "tram"),
                ("ref", "5"),
                ("operator", "ATAC"),
                ("network", "ATAC"),
            ],
            &[(Member::Way, wa, "")],
        );
        osm.relation(
            &[
                ("route", "tram"),
                ("ref", "5"),
                ("operator", "ATAC"),
                ("network", "ATAC-Nord"),
            ],
            &[(Member::Way, wb, "")],
        );
        osm.write(&clip);

        let fc = build_transit_lines(&clip, &gtfs, rome_driver().as_ref()).unwrap();
        let fives: Vec<&Value> = fc["features"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|f| f["properties"]["line"] == json!("5"))
            .collect();
        assert_eq!(
            fives.len(),
            2,
            "distinct networks stay as separate features"
        );
        let nets: HashSet<&str> = fives
            .iter()
            .map(|f| f["properties"]["network"].as_str().unwrap())
            .collect();
        assert_eq!(nets, HashSet::from(["ATAC", "ATAC-Nord"]));
    }

    /// Colliding sort keys (two networks sharing a ref+mode) sort to a stable
    /// total order regardless of the HashMap's run-to-run iteration order — the
    /// byte-reproducibility guard for the multi-network case (ADR 0029).
    #[test]
    fn sort_is_total_under_ref_collisions() {
        let mk = |kind: &str, line: &str, network: &str| json!({"properties":{"kind":kind,"line":line,"network":network}});
        // Same ref "5", same mode, two networks — the pre-2026 key collides.
        let mut a = vec![
            mk("tram", "5", "B-NET"),
            mk("tram", "5", "A-NET"),
            mk("metro", "A", "A-NET"),
        ];
        let mut b = vec![
            mk("metro", "A", "A-NET"),
            mk("tram", "5", "A-NET"),
            mk("tram", "5", "B-NET"),
        ];
        sort_metro_first(&mut a);
        sort_metro_first(&mut b);
        assert_eq!(a, b, "the order is independent of pre-sort arrangement");
        // metro first, then ref 5 with A-NET before B-NET (network tiebreaker).
        let nets: Vec<&str> = a
            .iter()
            .map(|f| f["properties"]["network"].as_str().unwrap())
            .collect();
        assert_eq!(nets, ["A-NET", "A-NET", "B-NET"]);
    }

    /// A `route_master` whose members disagree on ref/mode (malformed OSM)
    /// collapses deterministically to the lexicographically smallest identity
    /// rather than first-writer-wins, so its single feature is reproducible.
    #[test]
    fn mixed_master_collapses_deterministically() {
        let build = |order_swapped: bool| {
            let dir = tempfile::tempdir().unwrap();
            let clip = dir.path().join("clip.osm.pbf");
            let gtfs = dir.path().join("g.zip");
            write_gtfs(&gtfs, &[]);

            let mut osm = OsmBuilder::new();
            let n1 = osm.node(41.90, 12.49, &[]);
            let n2 = osm.node(41.91, 12.50, &[]);
            let n3 = osm.node(41.92, 12.51, &[]);
            let w7 = osm.way(&[n1, n2]);
            let wa = osm.way(&[n2, n3]);
            // A tram ref "7" and a metro ref "A" wrongly share one master.
            let tram = osm.relation(
                &[("route", "tram"), ("ref", "7"), ("operator", "ATAC")],
                &[(Member::Way, w7, "")],
            );
            let metro = osm.relation(
                &[("route", "subway"), ("ref", "A"), ("operator", "ATAC")],
                &[(Member::Way, wa, "")],
            );
            let members = if order_swapped {
                vec![(Member::Relation, metro, ""), (Member::Relation, tram, "")]
            } else {
                vec![(Member::Relation, tram, ""), (Member::Relation, metro, "")]
            };
            osm.relation(
                &[("type", "route_master"), ("route_master", "subway")],
                &members,
            );
            osm.write(&clip);

            let fc = build_transit_lines(&clip, &gtfs, rome_driver().as_ref()).unwrap();
            let feats = fc["features"].as_array().unwrap();
            assert_eq!(
                feats.len(),
                1,
                "the mixed master still collapses to one line"
            );
            (
                feats[0]["properties"]["kind"].as_str().unwrap().to_string(),
                feats[0]["properties"]["line"].as_str().unwrap().to_string(),
            )
        };
        // ("metro","A") < ("tram","7") lexicographically, so metro/A wins either
        // way the members were ordered.
        assert_eq!(build(false), ("metro".into(), "A".into()));
        assert_eq!(build(true), ("metro".into(), "A".into()));
    }

    /// Golden property-snapshot of a Rome-shaped nine-line build: the three metro
    /// lines (A/B/C, GTFS ids + colours, routable) plus six trams, in metro-first
    /// order. Guards the load-bearing "nine lines byte-stable" claim against any
    /// grouping/ordering/colour/identity regression (ADR 0029). Synthetic clip —
    /// one way per line keeps geometry out of the property assertions.
    #[test]
    fn rome_shaped_nine_lines_property_golden() {
        let dir = tempfile::tempdir().unwrap();
        let clip = dir.path().join("clip.osm.pbf");
        let gtfs = dir.path().join("g.zip");
        write_gtfs(
            &gtfs,
            &[
                ("MEA", "MEA", "1", "E27439"),
                ("MEB", "MEB", "1", "0570B5"),
                ("MEC", "MEC", "1", "008456"),
                ("R2", "2", "0", ""),
                ("R3", "3", "0", ""),
                ("R5", "5", "0", ""),
                ("R8", "8", "0", ""),
                ("R14", "14", "0", ""),
                ("R19", "19", "0", ""),
            ],
        );

        // The driver maps metro refs A/B/C → gtfs keys MEA/MEB/MEC.
        let metros = [("A", "subway"), ("B", "subway"), ("C", "subway")];
        let trams = [
            ("2", "tram"),
            ("3", "tram"),
            ("5", "tram"),
            ("8", "tram"),
            ("14", "tram"),
            ("19", "tram"),
        ];

        let mut osm = OsmBuilder::new();
        let mut lon = 12.40;
        let mut add = |osm: &mut OsmBuilder, refv: &str, route: &str| {
            let a = osm.node(41.90, lon, &[]);
            let b = osm.node(41.91, lon + 0.01, &[]);
            lon += 0.02;
            let w = osm.way(&[a, b]);
            osm.relation(
                &[("route", route), ("ref", refv), ("operator", "ATAC")],
                &[(Member::Way, w, "")],
            );
        };
        for (refv, route) in metros.iter().chain(trams.iter()) {
            add(&mut osm, refv, route);
        }
        osm.write(&clip);

        let fc = build_transit_lines(&clip, &gtfs, rome_driver().as_ref()).unwrap();
        // Snapshot the identity props of every feature in emit order.
        let shape: Vec<Value> = fc["features"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| {
                let p = &f["properties"];
                json!([p["kind"], p["line"], p["route"], p["color"], p["routable"]])
            })
            .collect();
        let expected = json!([
            ["metro", "A", "ATAC:MEA", "#E27439", true],
            ["metro", "B", "ATAC:MEB", "#0570B5", true],
            ["metro", "C", "ATAC:MEC", "#008456", true],
            ["tram", "2", "ATAC:R2", null, true],
            ["tram", "3", "ATAC:R3", null, true],
            ["tram", "5", "ATAC:R5", null, true],
            ["tram", "8", "ATAC:R8", null, true],
            ["tram", "14", "ATAC:R14", null, true],
            ["tram", "19", "ATAC:R19", null, true],
        ]);
        assert_eq!(Value::Array(shape), expected, "nine-line shape is stable");
        // Every feature carries the additive network prop.
        assert!(
            fc["features"]
                .as_array()
                .unwrap()
                .iter()
                .all(|f| f["properties"]["network"] == json!("ATAC")),
        );
    }

    // ── Minimal hand-rolled .osm.pbf writer (test fixtures only) ──────────────
    //
    // `osmpbf` is read-only, so we encode the protobuf wire format by hand: an
    // OSMHeader blob then one OSMData blob carrying a PrimitiveBlock with plain
    // Nodes/Ways/Relations (with tags + members). Just enough for the overlay
    // reader's passes; coords use the default granularity (100).

    #[derive(Clone, Copy)]
    enum Member {
        Node,
        Way,
        Relation,
    }

    /// Accumulates nodes/ways/relations and a shared string table, then encodes a
    /// single-block `.osm.pbf`.
    struct OsmBuilder {
        strings: Vec<String>,
        nodes: Vec<Vec<u8>>,
        ways: Vec<Vec<u8>>,
        relations: Vec<Vec<u8>>,
        next_id: i64,
    }

    impl OsmBuilder {
        fn new() -> Self {
            OsmBuilder {
                strings: vec![String::new()], // index 0 is the reserved blank
                nodes: Vec::new(),
                ways: Vec::new(),
                relations: Vec::new(),
                next_id: 1,
            }
        }

        fn intern(&mut self, s: &str) -> u32 {
            if let Some(i) = self.strings.iter().position(|x| x == s) {
                return i as u32;
            }
            self.strings.push(s.to_string());
            (self.strings.len() - 1) as u32
        }

        fn alloc(&mut self) -> i64 {
            let id = self.next_id;
            self.next_id += 1;
            id
        }

        /// Packed (key,val) string-id pairs for a tag list → (keys, vals).
        fn tag_cols(&mut self, tags: &[(&str, &str)]) -> (Vec<u64>, Vec<u64>) {
            let mut keys = Vec::new();
            let mut vals = Vec::new();
            for (k, v) in tags {
                keys.push(self.intern(k) as u64);
                vals.push(self.intern(v) as u64);
            }
            (keys, vals)
        }

        fn node(&mut self, lat: f64, lon: f64, tags: &[(&str, &str)]) -> i64 {
            let id = self.alloc();
            let (keys, vals) = self.tag_cols(tags);
            let mut n = Vec::new();
            field_varint(&mut n, 1, zigzag(id));
            if !keys.is_empty() {
                field_bytes(&mut n, 2, &packed(&keys));
                field_bytes(&mut n, 3, &packed(&vals));
            }
            field_varint(&mut n, 8, zigzag(stored(lat)));
            field_varint(&mut n, 9, zigzag(stored(lon)));
            self.nodes.push(n);
            id
        }

        fn way(&mut self, refs: &[i64]) -> i64 {
            self.way_tagged(refs, &[])
        }

        fn way_tagged(&mut self, refs: &[i64], tags: &[(&str, &str)]) -> i64 {
            let id = self.alloc();
            let (keys, vals) = self.tag_cols(tags);
            let mut w = Vec::new();
            field_varint(&mut w, 1, id as u64);
            if !keys.is_empty() {
                field_bytes(&mut w, 2, &packed(&keys));
                field_bytes(&mut w, 3, &packed(&vals));
            }
            let mut delta = Vec::new();
            let mut prev = 0i64;
            for &r in refs {
                delta.push(zigzag(r - prev));
                prev = r;
            }
            field_bytes(&mut w, 8, &packed(&delta));
            self.ways.push(w);
            id
        }

        fn relation(&mut self, tags: &[(&str, &str)], members: &[(Member, i64, &str)]) -> i64 {
            let id = self.alloc();
            let (keys, vals) = self.tag_cols(tags);
            let roles: Vec<u64> = members
                .iter()
                .map(|(_, _, role)| self.intern(role) as u64)
                .collect();
            let mut rel = Vec::new();
            field_varint(&mut rel, 1, id as u64);
            if !keys.is_empty() {
                field_bytes(&mut rel, 2, &packed(&keys));
                field_bytes(&mut rel, 3, &packed(&vals));
            }
            field_bytes(&mut rel, 8, &packed(&roles)); // roles_sid
            let mut memids = Vec::new();
            let mut prev = 0i64;
            for (_, mid, _) in members {
                memids.push(zigzag(mid - prev));
                prev = *mid;
            }
            field_bytes(&mut rel, 9, &packed(&memids));
            let types: Vec<u64> = members
                .iter()
                .map(|(t, _, _)| match t {
                    Member::Node => 0,
                    Member::Way => 1,
                    Member::Relation => 2,
                })
                .collect();
            field_bytes(&mut rel, 10, &packed(&types));
            self.relations.push(rel);
            id
        }

        fn write(&self, path: &Path) {
            let mut stringtable = Vec::new();
            for s in &self.strings {
                field_bytes(&mut stringtable, 1, s.as_bytes());
            }
            let mut group = Vec::new();
            for n in &self.nodes {
                field_bytes(&mut group, 1, n);
            }
            for w in &self.ways {
                field_bytes(&mut group, 3, w);
            }
            for r in &self.relations {
                field_bytes(&mut group, 4, r);
            }
            let mut block = Vec::new();
            field_bytes(&mut block, 1, &stringtable);
            field_bytes(&mut block, 2, &group);

            let mut header = Vec::new();
            field_bytes(&mut header, 4, b"OsmSchema-V0.6");

            let mut out = Vec::new();
            push_blob(&mut out, "OSMHeader", &header);
            push_blob(&mut out, "OSMData", &block);
            std::fs::write(path, out).unwrap();
        }
    }

    fn varint(out: &mut Vec<u8>, mut v: u64) {
        loop {
            let b = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                out.push(b);
                break;
            }
            out.push(b | 0x80);
        }
    }

    fn zigzag(v: i64) -> u64 {
        ((v << 1) ^ (v >> 63)) as u64
    }

    fn field_bytes(out: &mut Vec<u8>, field: u32, data: &[u8]) {
        varint(out, ((field as u64) << 3) | 2);
        varint(out, data.len() as u64);
        out.extend_from_slice(data);
    }

    fn field_varint(out: &mut Vec<u8>, field: u32, v: u64) {
        varint(out, (field as u64) << 3);
        varint(out, v);
    }

    fn packed(values: &[u64]) -> Vec<u8> {
        let mut b = Vec::new();
        for &v in values {
            varint(&mut b, v);
        }
        b
    }

    fn stored(deg: f64) -> i64 {
        (deg / 1e-7).round() as i64
    }

    fn push_blob(out: &mut Vec<u8>, blob_type: &str, payload: &[u8]) {
        let mut blob = Vec::new();
        field_bytes(&mut blob, 1, payload); // raw

        let mut blob_header = Vec::new();
        field_bytes(&mut blob_header, 1, blob_type.as_bytes()); // type
        field_varint(&mut blob_header, 3, blob.len() as u64); // datasize

        out.extend_from_slice(&(blob_header.len() as u32).to_be_bytes());
        out.extend_from_slice(&blob_header);
        out.extend_from_slice(&blob);
    }
}
