//! Rail-geometry shapes for the FL GTFS (extends ADR 0016). Per the design
//! (concept docs 25/26), a line's geometry comes from its OSM `route=train`
//! relation, NOT from per-trip GTFS shapes: the relation's member rail ways are
//! stitched into ONE continuous ordered polyline per branch, which GTFS then
//! carries as a `shapes.txt` row that every trip on that line points at via
//! `shape_id`.
//!
//! Two halves, split so the geometry is testable without a real PBF:
//! - [`stitch`] is a pure, I/O-free function over member ways — greedy endpoint
//!   joining with head/tail flipping, longest-run-wins on a gap, and a per-branch
//!   split. It panics on nothing and silently drops degenerate input.
//! - [`read_rail_shapes`] is the thin `osmpbf` adapter (mirroring the pipeline's
//!   overlay reader) that pulls `route=train` relations + their rail ways from an
//!   OSM clip and feeds them to [`stitch`]. It is fail-soft: an absent or
//!   unreadable clip yields no shapes, and the GTFS is emitted exactly as before.

use std::collections::HashMap;
use std::path::Path;

/// One member way of a route relation: its OSM id, ordered `(lon, lat)` points,
/// the relation role (`""` for the default track role), and the branch label the
/// way belongs to (the line `ref`, refined to a branch name where the relation
/// distinguishes them). Ways sharing a `branch` stitch together; different
/// branches become distinct polylines.
#[derive(Clone, Debug)]
pub struct MemberWay {
    pub way_id: i64,
    pub points: Vec<(f64, f64)>,
    pub role: String,
    pub branch: String,
}

/// A stitched, ordered polyline keyed by its branch label.
#[derive(Clone, Debug, PartialEq)]
pub struct Shape {
    pub branch: String,
    pub points: Vec<(f64, f64)>,
}

/// A rail member is one whose role is empty or doesn't mark a non-track member
/// (platforms, stops, stations). Mirrors the overlay reader's `platform` skip.
fn is_rail_role(role: &str) -> bool {
    !(role.starts_with("platform")
        || role.starts_with("stop")
        || role == "station"
        || role == "halt")
}

/// Stitch a relation's member ways into one ordered polyline per branch.
///
/// For each branch, rail ways are greedily chained at shared endpoint nodes,
/// flipping a way head-to-tail as needed to connect. When the remaining ways
/// can't extend the current run (a gap), a new run is started; the longest run
/// is kept and shorter fragments are dropped (fail-soft — a partial relation
/// still yields a usable line). Endpoints are matched on exact coordinate
/// equality, which is what shared OSM nodes give us.
pub fn stitch(members: &[MemberWay]) -> Vec<Shape> {
    // Group rail ways by branch, preserving first-seen branch order for a stable
    // shape ordering. A way id is taken at most once per branch (a relation can
    // list the same track way twice, e.g. shared between directions).
    let mut order: Vec<String> = Vec::new();
    let mut by_branch: HashMap<String, Vec<Vec<(f64, f64)>>> = HashMap::new();
    let mut seen: HashMap<String, std::collections::HashSet<i64>> = HashMap::new();
    for m in members {
        if !is_rail_role(&m.role) || m.points.len() < 2 {
            continue;
        }
        if !seen.entry(m.branch.clone()).or_default().insert(m.way_id) {
            continue; // way already taken for this branch
        }
        if !by_branch.contains_key(&m.branch) {
            order.push(m.branch.clone());
        }
        by_branch
            .entry(m.branch.clone())
            .or_default()
            .push(clean(&m.points));
    }

    let mut shapes = Vec::new();
    for branch in order {
        let ways = by_branch.remove(&branch).unwrap_or_default();
        if let Some(points) = stitch_one(ways) {
            shapes.push(Shape { branch, points });
        }
    }
    shapes
}

/// Drop consecutive duplicate points so endpoint matching and the GTFS output
/// don't carry zero-length steps.
fn clean(points: &[(f64, f64)]) -> Vec<(f64, f64)> {
    let mut out: Vec<(f64, f64)> = Vec::with_capacity(points.len());
    for &p in points {
        if out.last() != Some(&p) {
            out.push(p);
        }
    }
    out
}

/// Chain one branch's ways into the longest continuous run, returning its points
/// (or `None` if nothing usable remains).
fn stitch_one(mut ways: Vec<Vec<(f64, f64)>>) -> Option<Vec<(f64, f64)>> {
    ways.retain(|w| w.len() >= 2);
    if ways.is_empty() {
        return None;
    }

    let mut best: Vec<(f64, f64)> = Vec::new();
    // Build runs greedily until every way is consumed, keeping the longest.
    while let Some(seed) = ways.pop() {
        let mut run = seed;
        // Extend from both ends until no remaining way connects.
        while let Some((idx, append, flip)) = find_link(&run, &ways) {
            let mut way = ways.swap_remove(idx);
            if flip {
                way.reverse();
            }
            if append {
                // `way` starts at run's tail: skip its shared first point.
                run.extend(way.into_iter().skip(1));
            } else {
                // `way` ends at run's head: prepend, skip the shared last point.
                way.pop();
                let mut joined = way;
                joined.append(&mut run);
                run = joined;
            }
        }
        if run_len(&run) > run_len(&best) {
            best = run;
        }
    }

    (best.len() >= 2).then_some(best)
}

/// Find a remaining way that connects to either end of `run`. Returns its index,
/// whether it appends to the tail (`true`) or prepends to the head (`false`), and
/// whether the way must be reversed first.
fn find_link(run: &[(f64, f64)], ways: &[Vec<(f64, f64)>]) -> Option<(usize, bool, bool)> {
    let head = *run.first()?;
    let tail = *run.last()?;
    for (i, w) in ways.iter().enumerate() {
        let (ws, we) = (w[0], w[w.len() - 1]);
        // Append to the tail.
        if ws == tail {
            return Some((i, true, false));
        }
        if we == tail {
            return Some((i, true, true));
        }
        // Prepend to the head.
        if we == head {
            return Some((i, false, false));
        }
        if ws == head {
            return Some((i, false, true));
        }
    }
    None
}

/// Planar length of a polyline — only used to compare runs, so the cheap
/// degree-space metric is fine (no projection needed for "which is longer").
/// Must sum the per-segment Euclidean length, not its square: a sum of squares
/// is not monotonic with true length, so a sparse run with one long jump could
/// otherwise outrank a denser, geometrically longer run.
fn run_len(run: &[(f64, f64)]) -> f64 {
    run.windows(2)
        .map(|w| {
            let (dx, dy) = (w[1].0 - w[0].0, w[1].1 - w[0].1);
            dx.hypot(dy)
        })
        .sum()
}

/// Read `route=train` relations from an OSM clip and stitch each into its rail
/// shapes. Fail-soft by construction: a missing/unreadable clip or a clip with
/// no rail relations returns an empty list, so the caller emits GTFS unchanged.
///
/// `branch_of` maps a relation (by its `ref`/`name` tags) to the GTFS branch
/// label to group its ways under; returning `None` skips the relation entirely.
pub fn read_rail_shapes(clip: &Path, branch_of: impl Fn(&RelInfo) -> Option<String>) -> Vec<Shape> {
    match read_members(clip, &branch_of) {
        Ok(members) => stitch(&members),
        Err(e) => {
            tracing::debug!(error = %e, clip = %clip.display(), "fl-gtfs: no rail shapes from clip");
            Vec::new()
        }
    }
}

/// The relation tags the branch mapper sees: its `ref` and `name`.
pub struct RelInfo {
    pub route_ref: String,
    pub name: String,
}

fn read_members(
    clip: &Path,
    branch_of: &impl Fn(&RelInfo) -> Option<String>,
) -> anyhow::Result<Vec<MemberWay>> {
    use osmpbf::{Element, ElementReader, RelMemberType};

    // Pass 1: the route=train relations we keep, each as (branch, [(way_id,role)]).
    struct KeptRel {
        branch: String,
        ways: Vec<(i64, String)>,
    }
    let mut kept: Vec<KeptRel> = Vec::new();
    ElementReader::from_path(clip)?.for_each(|el| {
        if let Element::Relation(rel) = el {
            let mut route = None;
            let mut route_ref = String::new();
            let mut name = String::new();
            for (k, v) in rel.tags() {
                match k {
                    "route" => route = Some(v.to_string()),
                    "ref" => route_ref = v.to_string(),
                    "name" => name = v.to_string(),
                    _ => {}
                }
            }
            if route.as_deref() != Some("train") {
                return;
            }
            let Some(branch) = branch_of(&RelInfo { route_ref, name }) else {
                return;
            };
            let ways: Vec<(i64, String)> = rel
                .members()
                .filter(|m| m.member_type == RelMemberType::Way)
                .map(|m| (m.member_id, m.role().unwrap_or("").to_string()))
                .collect();
            if !ways.is_empty() {
                kept.push(KeptRel { branch, ways });
            }
        }
    })?;
    if kept.is_empty() {
        return Ok(Vec::new());
    }

    // Pass 2: ordered node refs for every needed way.
    let needed: std::collections::HashSet<i64> = kept
        .iter()
        .flat_map(|r| r.ways.iter().map(|(id, _)| *id))
        .collect();
    let mut way_nodes: HashMap<i64, Vec<i64>> = HashMap::new();
    ElementReader::from_path(clip)?.for_each(|el| {
        if let Element::Way(way) = el
            && needed.contains(&way.id())
        {
            way_nodes.insert(way.id(), way.refs().collect());
        }
    })?;

    // Pass 3: coords for every node those ways reference.
    let needed_nodes: std::collections::HashSet<i64> =
        way_nodes.values().flatten().copied().collect();
    let mut node_xy: HashMap<i64, (f64, f64)> = HashMap::new();
    ElementReader::from_path(clip)?.for_each(|el| match el {
        Element::Node(n) if needed_nodes.contains(&n.id()) => {
            node_xy.insert(n.id(), (n.lon(), n.lat()));
        }
        Element::DenseNode(n) if needed_nodes.contains(&n.id()) => {
            node_xy.insert(n.id(), (n.lon(), n.lat()));
        }
        _ => {}
    })?;

    // Resolve each kept relation's ways into MemberWays.
    let mut members = Vec::new();
    for rel in &kept {
        for (way_id, role) in &rel.ways {
            let Some(nodes) = way_nodes.get(way_id) else {
                continue;
            };
            let points: Vec<(f64, f64)> = nodes
                .iter()
                .filter_map(|n| node_xy.get(n).copied())
                .collect();
            members.push(MemberWay {
                way_id: *way_id,
                points,
                role: role.clone(),
                branch: rel.branch.clone(),
            });
        }
    }
    Ok(members)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn way(id: i64, pts: &[(f64, f64)], branch: &str) -> MemberWay {
        MemberWay {
            way_id: id,
            points: pts.to_vec(),
            role: String::new(),
            branch: branch.to_string(),
        }
    }

    #[test]
    fn simple_chain_joins_in_order() {
        // Two ways meeting at (1,0): A=[(0,0),(1,0)], B=[(1,0),(2,0)].
        let m = vec![
            way(1, &[(0.0, 0.0), (1.0, 0.0)], "L"),
            way(2, &[(1.0, 0.0), (2.0, 0.0)], "L"),
        ];
        let shapes = stitch(&m);
        assert_eq!(shapes.len(), 1);
        assert!(same_polyline(
            &shapes[0].points,
            &[(0.0, 0.0), (1.0, 0.0), (2.0, 0.0)]
        ));
    }

    /// The stitched orientation is arbitrary (it depends on the seed), so compare
    /// against the expected polyline in either direction.
    fn same_polyline(got: &[(f64, f64)], want: &[(f64, f64)]) -> bool {
        let rev: Vec<(f64, f64)> = want.iter().rev().copied().collect();
        got == want || got == rev.as_slice()
    }

    #[test]
    fn reversed_way_is_flipped_to_connect() {
        // B is stored tail-first; the stitcher must flip it to join A's tail.
        let m = vec![
            way(1, &[(0.0, 0.0), (1.0, 0.0)], "L"),
            way(2, &[(2.0, 0.0), (1.0, 0.0)], "L"),
        ];
        let shapes = stitch(&m);
        assert_eq!(shapes.len(), 1);
        assert!(same_polyline(
            &shapes[0].points,
            &[(0.0, 0.0), (1.0, 0.0), (2.0, 0.0)]
        ));
    }

    #[test]
    fn out_of_order_seed_extends_from_the_head() {
        // The seed popped last is the middle way; the others must prepend/append.
        let m = vec![
            way(1, &[(0.0, 0.0), (1.0, 0.0)], "L"),
            way(2, &[(1.0, 0.0), (2.0, 0.0)], "L"),
            way(3, &[(2.0, 0.0), (3.0, 0.0)], "L"),
        ];
        let shapes = stitch(&m);
        assert_eq!(shapes.len(), 1);
        assert!(same_polyline(
            &shapes[0].points,
            &[(0.0, 0.0), (1.0, 0.0), (2.0, 0.0), (3.0, 0.0)]
        ));
    }

    #[test]
    fn a_gap_keeps_the_longest_run() {
        // Run X = three ways (0..3); run Y = one short way far away. Longest wins.
        let m = vec![
            way(1, &[(0.0, 0.0), (1.0, 0.0)], "L"),
            way(2, &[(1.0, 0.0), (2.0, 0.0)], "L"),
            way(3, &[(2.0, 0.0), (3.0, 0.0)], "L"),
            way(9, &[(100.0, 0.0), (100.5, 0.0)], "L"),
        ];
        let shapes = stitch(&m);
        assert_eq!(shapes.len(), 1, "one shape per branch");
        assert!(
            same_polyline(
                &shapes[0].points,
                &[(0.0, 0.0), (1.0, 0.0), (2.0, 0.0), (3.0, 0.0)]
            ),
            "the long run is kept, the stray fragment dropped"
        );
    }

    #[test]
    fn a_y_branch_splits_by_branch_label() {
        // A shared trunk plus two spurs, tagged as two branches → two shapes.
        let m = vec![
            way(1, &[(0.0, 0.0), (1.0, 0.0)], "main"),
            way(2, &[(1.0, 0.0), (2.0, 1.0)], "main"),
            way(3, &[(1.0, 0.0), (2.0, -1.0)], "spur"),
        ];
        let shapes = stitch(&m);
        assert_eq!(shapes.len(), 2);
        let main = shapes.iter().find(|s| s.branch == "main").unwrap();
        assert!(same_polyline(
            &main.points,
            &[(0.0, 0.0), (1.0, 0.0), (2.0, 1.0)]
        ));
        let spur = shapes.iter().find(|s| s.branch == "spur").unwrap();
        // Assert the spur's geometry, not just its length, so a mis-grouped or
        // mangled way fails the test.
        assert!(same_polyline(&spur.points, &[(1.0, 0.0), (2.0, -1.0)]));
    }

    #[test]
    fn non_rail_and_degenerate_members_are_skipped() {
        let m = vec![
            MemberWay {
                way_id: 1,
                points: vec![(0.0, 0.0), (1.0, 0.0)],
                role: "platform".to_string(),
                branch: "L".to_string(),
            },
            MemberWay {
                way_id: 2,
                points: vec![(5.0, 5.0)], // single point — degenerate
                role: String::new(),
                branch: "L".to_string(),
            },
            way(3, &[(1.0, 0.0), (2.0, 0.0)], "L"),
        ];
        let shapes = stitch(&m);
        // Only the lone rail way survives; it alone is a 2-point shape.
        assert_eq!(shapes.len(), 1);
        assert_eq!(shapes[0].points, vec![(1.0, 0.0), (2.0, 0.0)]);
    }

    #[test]
    fn empty_input_yields_no_shapes() {
        assert!(stitch(&[]).is_empty());
    }

    #[test]
    fn duplicate_points_are_collapsed() {
        let m = vec![way(1, &[(0.0, 0.0), (0.0, 0.0), (1.0, 0.0)], "L")];
        let shapes = stitch(&m);
        assert_eq!(shapes[0].points, vec![(0.0, 0.0), (1.0, 0.0)]);
    }

    #[test]
    fn a_repeated_way_id_is_taken_once_per_branch() {
        // The same track way listed twice (shared between directions) must not be
        // chained onto itself — the result is the single 2-point way.
        let m = vec![
            way(1, &[(0.0, 0.0), (1.0, 0.0)], "L"),
            way(1, &[(0.0, 0.0), (1.0, 0.0)], "L"),
        ];
        let shapes = stitch(&m);
        assert_eq!(shapes.len(), 1);
        assert_eq!(shapes[0].points, vec![(0.0, 0.0), (1.0, 0.0)]);
    }

    #[test]
    fn longest_run_uses_true_length_not_squared_segments() {
        // Run A is one long jump (length 10); run B is many short colinear hops
        // (length 12, but a smaller sum of squared segments). True length must
        // win, so B is kept — a squared-length metric would wrongly pick A.
        let m = vec![
            way(1, &[(0.0, 0.0), (10.0, 0.0)], "L"),
            way(2, &[(0.0, 100.0), (3.0, 100.0)], "L"),
            way(3, &[(3.0, 100.0), (6.0, 100.0)], "L"),
            way(4, &[(6.0, 100.0), (9.0, 100.0)], "L"),
            way(5, &[(9.0, 100.0), (12.0, 100.0)], "L"),
        ];
        let shapes = stitch(&m);
        assert_eq!(shapes.len(), 1);
        assert!(
            same_polyline(
                &shapes[0].points,
                &[
                    (0.0, 100.0),
                    (3.0, 100.0),
                    (6.0, 100.0),
                    (9.0, 100.0),
                    (12.0, 100.0)
                ]
            ),
            "the geometrically longer run wins, not the higher sum-of-squares one"
        );
    }

    #[test]
    fn closed_loop_way_terminates_and_is_panic_free() {
        // A self-closing ring and a two-way cycle are realistic for rail loops;
        // the greedy stitcher must terminate and still yield a usable polyline.
        let ring = vec![way(
            1,
            &[(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 0.0)],
            "L",
        )];
        let shapes = stitch(&ring);
        assert_eq!(shapes.len(), 1);
        assert!(shapes[0].points.len() >= 2);

        let cycle = vec![
            way(1, &[(0.0, 0.0), (1.0, 0.0)], "L"),
            way(2, &[(1.0, 0.0), (0.0, 0.0)], "L"),
        ];
        let shapes = stitch(&cycle);
        assert_eq!(shapes.len(), 1);
        assert!(shapes[0].points.len() >= 2);
    }

    #[test]
    fn missing_clip_is_fail_soft() {
        let shapes = read_rail_shapes(Path::new("/no/such/clip.osm.pbf"), |_| {
            Some("L".to_string())
        });
        assert!(shapes.is_empty());
    }

    #[test]
    fn corrupt_clip_is_fail_soft() {
        // A present-but-garbage PBF is the realistic "unreadable clip" case: the
        // reader must return no shapes without panicking, not just for ENOENT.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("junk.osm.pbf");
        std::fs::write(&path, b"this is not a pbf, just junk bytes").unwrap();
        let shapes = read_rail_shapes(&path, |_| Some("L".to_string()));
        assert!(shapes.is_empty());
    }

    #[test]
    fn reads_and_stitches_a_tiny_rail_clip() {
        // End-to-end over a real (tiny) .osm.pbf: a route=train relation with two
        // rail ways sharing a node must resolve relation→way→node and stitch into
        // one branch polyline. Exercises the 3-pass reader and the Node path.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("rail.osm.pbf");
        write_tiny_rail_pbf(&path);

        let shapes = read_rail_shapes(&path, |r: &RelInfo| {
            (r.route_ref == "FL1").then(|| "FL1".to_string())
        });
        assert_eq!(shapes.len(), 1, "one stitched shape for the route");
        assert_eq!(shapes[0].branch, "FL1");
        assert!(
            same_polyline(
                &shapes[0].points,
                &[(12.0, 41.0), (12.0, 42.0), (13.0, 42.0)]
            ),
            "two ways joined at the shared node: {:?}",
            shapes[0].points
        );
    }

    // --- Minimal hand-rolled .osm.pbf writer (test fixtures only) -------------
    //
    // `osmpbf` is read-only, so we encode the protobuf wire format by hand: a
    // length-prefixed OSMHeader blob followed by an OSMData blob carrying one
    // PrimitiveBlock with plain Nodes, Ways, and one Relation. Just enough for
    // the reader's 3-pass resolution; coords use the default granularity (100).

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

    /// Field with wire type 2 (length-delimited): tag, length, then bytes.
    fn field_bytes(out: &mut Vec<u8>, field: u32, data: &[u8]) {
        varint(out, ((field as u64) << 3) | 2);
        varint(out, data.len() as u64);
        out.extend_from_slice(data);
    }

    /// Field with wire type 0 (varint); the type bits are zero, so the tag is
    /// just the field number shifted left by 3.
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

    /// Encode lat/lon degrees to the stored value for granularity 100 (1e-7/unit).
    fn stored(deg: f64) -> i64 {
        (deg / 1e-7).round() as i64
    }

    /// Write a minimal `.osm.pbf` with one `route=train` relation (`ref=FL1`) and
    /// two rail ways sharing node 2, so the reader has a real fixture to resolve.
    fn write_tiny_rail_pbf(path: &Path) {
        // String table: index 0 is the reserved blank delimiter.
        let strings: &[&str] = &["", "route", "train", "ref", "FL1"];
        let (s_route, s_train, s_ref, s_fl1) = (1u32, 2u32, 3u32, 4u32);

        let mut stringtable = Vec::new();
        for s in strings {
            field_bytes(&mut stringtable, 1, s.as_bytes());
        }

        // Three plain nodes (1,2,3). Node.id=1 (sint64), lat=8, lon=9 (sint64).
        let node = |id: i64, lat: f64, lon: f64| {
            let mut n = Vec::new();
            field_varint(&mut n, 1, zigzag(id));
            field_varint(&mut n, 8, zigzag(stored(lat)));
            field_varint(&mut n, 9, zigzag(stored(lon)));
            n
        };
        let nodes = [
            node(1, 41.0, 12.0),
            node(2, 42.0, 12.0),
            node(3, 42.0, 13.0),
        ];

        // Two ways: refs are delta-coded sint64. Way 10 = nodes 1,2; way 11 = 2,3.
        let way = |id: i64, refs: &[i64]| {
            let mut w = Vec::new();
            field_varint(&mut w, 1, id as u64);
            let mut delta = Vec::new();
            let mut prev = 0i64;
            for &r in refs {
                delta.push(zigzag(r - prev));
                prev = r;
            }
            field_bytes(&mut w, 8, &packed(&delta));
            w
        };
        let ways = [way(10, &[1, 2]), way(11, &[2, 3])];

        // One relation: tags route=train, ref=FL1; two Way members (ids 10,11).
        let mut rel = Vec::new();
        field_varint(&mut rel, 1, 100); // id
        field_bytes(&mut rel, 2, &packed(&[s_route as u64, s_ref as u64])); // keys
        field_bytes(&mut rel, 3, &packed(&[s_train as u64, s_fl1 as u64])); // vals
        field_bytes(&mut rel, 8, &packed(&[0, 0])); // roles_sid (blank)
        // memids delta-coded: 10, then +1.
        field_bytes(&mut rel, 9, &packed(&[zigzag(10), zigzag(1)]));
        field_bytes(&mut rel, 10, &packed(&[1, 1])); // types: WAY, WAY

        // One PrimitiveGroup carrying nodes(1), ways(3), relations(4).
        let mut group = Vec::new();
        for n in &nodes {
            field_bytes(&mut group, 1, n);
        }
        for w in &ways {
            field_bytes(&mut group, 3, w);
        }
        field_bytes(&mut group, 4, &rel);

        // PrimitiveBlock: stringtable(1), primitivegroup(2).
        let mut block = Vec::new();
        field_bytes(&mut block, 1, &stringtable);
        field_bytes(&mut block, 2, &group);

        // OSMHeader block: required_features(4) = "OsmSchema-V0.6".
        let mut header = Vec::new();
        field_bytes(&mut header, 4, b"OsmSchema-V0.6");

        let mut out = Vec::new();
        push_blob(&mut out, "OSMHeader", &header);
        push_blob(&mut out, "OSMData", &block);
        std::fs::write(path, out).unwrap();
    }

    /// Frame one fileblock: BlobHeader (length-prefixed, big-endian) then the
    /// uncompressed Blob carrying `payload` in its `raw` field.
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
