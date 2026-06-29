//! STYLES — render the four whitelisted MapLibre styles (Standard / Transit ×
//! light / dark) into `output/styles/`, wired to the region's tile source, the
//! glyph endpoint, the sprite (Standard only), and the region's declared transit
//! overlay sources. Every host-absolute URL is the literal `__BASE_URL__` token
//! the gateway rewrites per request (ADR 0025).
//!
//! The styles are byte-stable: each is built as a `serde_json::Value` and
//! serialized with sorted keys, so a re-run reproduces the exact bytes the
//! gateway serves. Skip-if-present (all four parse); `FORCE_STYLES`.

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::context::Context;
use crate::fsx;
use crate::step::Step;

/// The four styles the gateway whitelists, paired with their mode + theme. Keep
/// the names in lockstep with `iter_contracts::offline::STYLE_WHITELIST`.
const STYLES: [(&str, Mode, Theme); 4] = [
    ("light", Mode::Standard, Theme::Light),
    ("dark", Mode::Standard, Theme::Dark),
    ("transit-light", Mode::Transit, Theme::Light),
    ("transit-dark", Mode::Transit, Theme::Dark),
];

#[derive(Clone, Copy, PartialEq)]
enum Mode {
    /// Driving/walking: full road palette, road shields.
    Standard,
    /// Transit backdrop: roads collapsed toward the background, no shields.
    Transit,
}

#[derive(Clone, Copy)]
enum Theme {
    Light,
    Dark,
}

pub struct RenderStyles;

#[async_trait]
impl Step for RenderStyles {
    fn name(&self) -> &'static str {
        "STYLES"
    }

    async fn satisfied(&self, ctx: &Context) -> bool {
        for (name, _, _) in STYLES {
            let path = ctx.output(&format!("output/styles/{name}.json"));
            let ok = tokio::fs::read(&path)
                .await
                .ok()
                .filter(|b| !b.is_empty())
                .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
                .is_some();
            if !ok {
                return false;
            }
        }
        true
    }

    async fn run(&self, ctx: &Context) -> anyhow::Result<()> {
        let tiles_basename = ctx.tiles_filename();
        let overlays = overlay_kinds(ctx);
        tokio::fs::create_dir_all(ctx.output("output/styles")).await?;

        for (name, mode, theme) in STYLES {
            let style = render_style(name, mode, theme, &tiles_basename, &overlays);
            // Pretty + trailing newline: byte-stable (serde_json sorts keys) and
            // a clean file the offline bundler copies verbatim.
            let mut bytes = serde_json::to_vec_pretty(&style)?;
            bytes.push(b'\n');
            fsx::write_atomic(&ctx.output(&format!("output/styles/{name}.json")), &bytes).await?;
        }
        tracing::info!(count = STYLES.len(), "rendered MapLibre styles");
        Ok(())
    }
}

/// The region's declared overlay kinds the transit styles wire as GeoJSON
/// sources (`metro-stations`, `transit-lines`). Filtered through the same
/// implemented set OVERLAY builds, so STYLES never references a `.geojson` the
/// pipeline doesn't emit. Empty for a region with no (implemented) overlays —
/// the style then carries the tile source alone.
fn overlay_kinds(ctx: &Context) -> Vec<String> {
    ctx.region
        .overlays
        .iter()
        .map(|o| o.kind.clone())
        .filter(|k| crate::steps::IMPLEMENTED_OVERLAY_KINDS.contains(&k.as_str()))
        .collect()
}

/// Build one MapLibre style document (spec version 8) for `mode`/`theme`.
fn render_style(
    name: &str,
    mode: Mode,
    theme: Theme,
    tiles_basename: &str,
    overlays: &[String],
) -> Value {
    let pal = palette(mode, theme);

    let mut sources = serde_json::Map::new();
    sources.insert(
        "openmaptiles".to_string(),
        json!({
            "type": "vector",
            "url": format!("pmtiles://__BASE_URL__/tiles/{tiles_basename}"),
        }),
    );
    // Transit styles back the app's overlays; wire the region's declared overlay
    // GeoJSON endpoints as sources so the style travels with its region.
    if mode == Mode::Transit {
        for kind in overlays {
            sources.insert(
                format!("overlay-{kind}"),
                json!({
                    "type": "geojson",
                    "data": format!("__BASE_URL__/overlays/{kind}.geojson"),
                }),
            );
        }
    }

    let mut style = json!({
        "version": 8,
        "name": format!("iter-{name}"),
        "metadata": {
            "iter:description": description(mode, theme),
            "iter:palette": pal.label,
        },
        "glyphs": "__BASE_URL__/glyphs/{fontstack}/{range}.pbf",
        "sources": Value::Object(sources),
        "layers": layers(mode, &pal),
    });

    // Standard styles carry the road-shield sprite; transit styles omit it.
    if mode == Mode::Standard {
        style["sprite"] = json!("__BASE_URL__/sprite/sprite");
    }
    style
}

fn description(mode: Mode, theme: Theme) -> &'static str {
    match (mode, theme) {
        (Mode::Standard, Theme::Light) => "Standard light — warm greige, road shields",
        (Mode::Standard, Theme::Dark) => "Standard dark — cold charcoal, road shields",
        (Mode::Transit, Theme::Light) => "Transit light — dimmed backdrop, no shields",
        (Mode::Transit, Theme::Dark) => "Transit dark — deep cold backdrop, no shields",
    }
}

/// The per-mode/theme colour set. Values track the approved look; they are
/// tunable, but the *structure* (one accent on Standard trunk, collapsed casings
/// on Transit) is the design intent.
struct Palette {
    label: &'static str,
    background: &'static str,
    grass: &'static str,
    park: &'static str,
    residential: &'static str,
    water: &'static str,
    waterway: &'static str,
    building: &'static str,
    /// Road fills by class, minor→trunk.
    road_minor: &'static str,
    road_tertiary: &'static str,
    road_secondary: &'static str,
    road_primary: &'static str,
    road_trunk: &'static str,
    /// Casing under the road fills. Transit collapses every class to this one.
    casing: &'static str,
    railway: &'static str,
    boundary: &'static str,
    label_text: &'static str,
    label_halo: &'static str,
}

fn palette(mode: Mode, theme: Theme) -> Palette {
    match (mode, theme) {
        (Mode::Standard, Theme::Light) => Palette {
            label: "standard-light-greige",
            background: "#E7E0D3",
            grass: "#D9E1CD",
            park: "#CEDCBE",
            residential: "#E8E2D8",
            water: "#BCD6E6",
            waterway: "#BCD6E6",
            building: "#DAD2C3",
            road_minor: "#FFFFFF",
            road_tertiary: "#FFFFFF",
            road_secondary: "#FFFBF1",
            road_primary: "#FCEBC2",
            road_trunk: "#F7DC98",
            casing: "#D8CFBE",
            railway: "#D8CEC0",
            boundary: "#A89F8E",
            label_text: "#4A453E",
            label_halo: "#FFFFFF",
        },
        (Mode::Standard, Theme::Dark) => Palette {
            label: "standard-dark-charcoal",
            background: "#23272E",
            grass: "#272C32",
            park: "#2A3030",
            residential: "#262A30",
            water: "#213642",
            waterway: "#213642",
            building: "#2B2F36",
            road_minor: "#3B4049",
            road_tertiary: "#454B54",
            road_secondary: "#525965",
            road_primary: "#5F6772",
            road_trunk: "#6E7886",
            casing: "#2A2E35",
            railway: "#3F454E",
            boundary: "#4B515A",
            label_text: "#DDE1E7",
            label_halo: "#23272E",
        },
        (Mode::Transit, Theme::Light) => Palette {
            label: "transit-light-dimmed",
            background: "#DEDDD8",
            grass: "#D7DAD0",
            park: "#D2D8C9",
            residential: "#DEDDD8",
            water: "#C7D4DB",
            waterway: "#C7D4DB",
            building: "#D4D2CB",
            // Road fills compress to a tight grey ramp — the warm-yellow trunk is gone.
            road_minor: "#EAE9E4",
            road_tertiary: "#E6E5E0",
            road_secondary: "#E2E1DC",
            road_primary: "#DEDDD8",
            road_trunk: "#DAD9D4",
            casing: "#D2D1CC",
            railway: "#CBC6BC",
            boundary: "#B6B4AD",
            label_text: "#6F6E69",
            label_halo: "#DEDDD8",
        },
        (Mode::Transit, Theme::Dark) => Palette {
            label: "transit-dark-deep-cold",
            background: "#191D26",
            grass: "#1C2029",
            park: "#1E2330",
            residential: "#191D26",
            water: "#172A36",
            waterway: "#172A36",
            building: "#1E222B",
            road_minor: "#262B34",
            road_tertiary: "#2A2F39",
            road_secondary: "#2F343E",
            road_primary: "#333944",
            road_trunk: "#373D49",
            casing: "#1E222B",
            railway: "#3A4049",
            boundary: "#363B45",
            label_text: "#A8AEB8",
            label_halo: "#191D26",
        },
    }
}

/// The shared layer stack (bottom → top). Standard appends `road-shield`.
fn layers(mode: Mode, pal: &Palette) -> Value {
    let mut layers = vec![
        json!({
            "id": "background",
            "type": "background",
            "paint": { "background-color": pal.background },
        }),
        json!({
            "id": "landcover-grass",
            "type": "fill",
            "source": "openmaptiles",
            "source-layer": "landcover",
            "filter": ["match", ["get", "class"], ["grass", "scrub", "wood", "forest"], true, false],
            "paint": { "fill-color": pal.grass, "fill-opacity": 0.7 },
        }),
        json!({
            "id": "landuse-park",
            "type": "fill",
            "source": "openmaptiles",
            "source-layer": "park",
            "paint": { "fill-color": pal.park, "fill-opacity": 0.8 },
        }),
        json!({
            "id": "landuse-residential",
            "type": "fill",
            "source": "openmaptiles",
            "source-layer": "landuse",
            "filter": ["==", ["get", "class"], "residential"],
            "paint": { "fill-color": pal.residential, "fill-opacity": 0.6 },
        }),
        json!({
            "id": "water",
            "type": "fill",
            "source": "openmaptiles",
            "source-layer": "water",
            "paint": { "fill-color": pal.water },
        }),
        json!({
            "id": "waterway",
            "type": "line",
            "source": "openmaptiles",
            "source-layer": "waterway",
            "paint": {
                "line-color": pal.waterway,
                "line-width": ["interpolate", ["linear"], ["zoom"], 8, 0.5, 14, 1.5],
            },
        }),
        json!({
            "id": "building",
            "type": "fill",
            "source": "openmaptiles",
            "source-layer": "building",
            "minzoom": 13,
            "paint": {
                "fill-color": pal.building,
                "fill-opacity": ["interpolate", ["linear"], ["zoom"], 13, 0.0, 14, 0.7],
            },
        }),
    ];

    // Roads: casing then fill per class. Transit collapses every casing to one
    // value (no class differentiation); Standard tints casings near-background.
    for road in &road_classes() {
        layers.push(road_casing(road, pal));
        layers.push(road_fill(road, pal));
    }

    layers.push(json!({
        "id": "railway",
        "type": "line",
        "source": "openmaptiles",
        "source-layer": "transportation",
        "minzoom": 10,
        "filter": ["==", ["get", "class"], "rail"],
        "paint": {
            "line-color": pal.railway,
            "line-width": ["interpolate", ["linear"], ["zoom"], 10, 0.6, 14, 1.4],
            "line-dasharray": [4, 2],
            "line-blur": 0.8,
        },
    }));
    layers.push(json!({
        "id": "boundary-country",
        "type": "line",
        "source": "openmaptiles",
        "source-layer": "boundary",
        "filter": ["==", ["get", "admin_level"], 2],
        "paint": { "line-color": pal.boundary, "line-dasharray": [3, 2], "line-width": 1.0 },
    }));
    layers.push(json!({
        "id": "place-city",
        "type": "symbol",
        "source": "openmaptiles",
        "source-layer": "place",
        "filter": ["match", ["get", "class"], ["city", "town"], true, false],
        "layout": {
            "text-field": ["get", "name:it"],
            "text-font": ["NotoSans-Regular"],
            "text-size": ["interpolate", ["linear"], ["zoom"], 6, 11, 14, 16],
        },
        "paint": {
            "text-color": pal.label_text,
            "text-halo-color": pal.label_halo,
            "text-halo-width": 1.2,
        },
    }));
    layers.push(json!({
        "id": "place-suburb",
        "type": "symbol",
        "source": "openmaptiles",
        "source-layer": "place",
        "minzoom": 11,
        "filter": ["match", ["get", "class"], ["suburb", "neighbourhood", "village"], true, false],
        "layout": {
            "text-field": ["get", "name:it"],
            "text-font": ["NotoSans-Regular"],
            "text-size": 11,
        },
        "paint": {
            "text-color": pal.label_text,
            "text-halo-color": pal.label_halo,
            "text-halo-width": 1.0,
        },
    }));
    layers.push(json!({
        "id": "road-label",
        "type": "symbol",
        "source": "openmaptiles",
        "source-layer": "transportation_name",
        "minzoom": 13,
        "layout": {
            "text-field": ["get", "name"],
            "text-font": ["NotoSans-Regular"],
            "text-size": 10,
            "symbol-placement": "line",
        },
        "paint": {
            "text-color": pal.label_text,
            "text-halo-color": pal.label_halo,
            "text-halo-width": 1.0,
        },
    }));

    if mode == Mode::Standard {
        layers.push(road_shield());
    }

    Value::Array(layers)
}

/// One road class: its source filter, fill colour key, casing+fill widths, and
/// the zoom it appears at.
struct RoadClass {
    id: &'static str,
    /// OMT `class` values this layer draws.
    classes: &'static [&'static str],
    minzoom: u8,
    /// Fill width at z8 and z18 (exponential interpolation between).
    fill_w8: f64,
    fill_w18: f64,
}

fn road_classes() -> [RoadClass; 5] {
    [
        RoadClass {
            id: "minor",
            classes: &["minor", "service", "track"],
            minzoom: 13,
            fill_w8: 0.5,
            fill_w18: 6.0,
        },
        RoadClass {
            id: "tertiary",
            classes: &["tertiary"],
            minzoom: 10,
            fill_w8: 0.8,
            fill_w18: 9.0,
        },
        RoadClass {
            id: "secondary",
            classes: &["secondary"],
            minzoom: 8,
            fill_w8: 1.0,
            fill_w18: 11.0,
        },
        RoadClass {
            id: "primary",
            classes: &["primary"],
            minzoom: 7,
            fill_w8: 1.2,
            fill_w18: 13.0,
        },
        RoadClass {
            id: "trunk",
            classes: &["trunk", "motorway"],
            minzoom: 5,
            fill_w8: 1.6,
            fill_w18: 16.0,
        },
    ]
}

fn road_filter(road: &RoadClass) -> Value {
    let classes: Vec<Value> = road.classes.iter().map(|c| json!(c)).collect();
    json!(["match", ["get", "class"], classes, true, false])
}

fn road_width(w8: f64, w18: f64) -> Value {
    json!([
        "interpolate",
        ["exponential", 1.5],
        ["zoom"],
        8,
        w8,
        18,
        w18
    ])
}

fn road_casing(road: &RoadClass, pal: &Palette) -> Value {
    json!({
        "id": format!("road-{}-casing", road.id),
        "type": "line",
        "source": "openmaptiles",
        "source-layer": "transportation",
        "minzoom": road.minzoom,
        "filter": road_filter(road),
        "layout": { "line-join": "round", "line-cap": "round" },
        "paint": {
            "line-color": pal.casing,
            "line-width": road_width(road.fill_w8 + 1.0, road.fill_w18 + 2.0),
            "line-blur": 0.8,
        },
    })
}

fn road_fill(road: &RoadClass, pal: &Palette) -> Value {
    // The palette ramps fill colour by class; Transit palettes compress the ramp
    // toward the background while keeping the per-class width so the network still
    // reads, just dimmed.
    let color = match road.id {
        "minor" => pal.road_minor,
        "tertiary" => pal.road_tertiary,
        "secondary" => pal.road_secondary,
        "primary" => pal.road_primary,
        _ => pal.road_trunk,
    };
    json!({
        "id": format!("road-{}-fill", road.id),
        "type": "line",
        "source": "openmaptiles",
        "source-layer": "transportation",
        "minzoom": road.minzoom,
        "filter": road_filter(road),
        "layout": { "line-join": "round", "line-cap": "round" },
        "paint": {
            "line-color": color,
            "line-width": road_width(road.fill_w8, road.fill_w18),
            "line-blur": 0.8,
        },
    })
}

/// The Standard-only road-shield symbol layer: A-road/GRA badges from the sprite
/// with the `ref` composited over them. Absent from transit styles.
fn road_shield() -> Value {
    json!({
        "id": "road-shield",
        "type": "symbol",
        "source": "openmaptiles",
        "source-layer": "transportation_name",
        "minzoom": 8,
        "filter": [
            "all",
            ["has", "ref"],
            ["match", ["get", "class"], ["motorway", "trunk", "primary"], true, false],
            ["==", ["get", "ref"], ["upcase", ["get", "ref"]]],
            ["==", ["index-of", "BIS", ["get", "ref"]], -1],
            ["==", ["index-of", "DIR", ["get", "ref"]], -1],
            ["==", ["index-of", "VAR", ["get", "ref"]], -1],
            ["==", ["index-of", "RAC", ["get", "ref"]], -1]
        ],
        "layout": {
            "symbol-placement": "line",
            "symbol-spacing": 400,
            "icon-image": [
                "case",
                [
                    "any",
                    ["==", ["index-of", "A", ["get", "ref"]], 0],
                    ["==", ["index-of", "RA", ["get", "ref"]], 0],
                    ["==", ["index-of", "GRA", ["get", "ref"]], 0]
                ],
                "shield-motorway",
                "shield-primary"
            ],
            "icon-text-fit": "both",
            "icon-text-fit-padding": [1, 2, 1, 2],
            "icon-rotation-alignment": "viewport",
            "text-rotation-alignment": "viewport",
            "text-field": ["get", "ref"],
            "text-font": ["NotoSans-Regular"],
            "text-size": 9
        },
        "paint": {
            "text-color": "#FFFFFF",
            "text-halo-color": "#FFFFFF",
            "text-halo-width": 0.5
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::path::PathBuf;

    fn ctx(dir: &std::path::Path) -> Context {
        Context::for_test(dir.to_path_buf(), "test")
    }

    /// Every whitelisted style renders, parses, and carries the required keys.
    #[test]
    fn each_style_parses_with_required_keys() {
        for (name, mode, theme) in STYLES {
            let v = render_style(
                name,
                mode,
                theme,
                "rome.pmtiles",
                &["metro-stations".to_string()],
            );
            assert_eq!(v["version"], 8, "{name}: spec version 8");
            assert!(v["glyphs"].is_string(), "{name}: has glyphs");
            assert!(v["sources"].is_object(), "{name}: has sources");
            assert!(v["layers"].is_array(), "{name}: has layers");
            assert!(
                !v["layers"].as_array().unwrap().is_empty(),
                "{name}: non-empty layers"
            );
            // Sprite is required on Standard (road shields) and absent on Transit;
            // assert it for all four uniformly rather than relying on spot checks.
            match mode {
                Mode::Standard => assert!(v["sprite"].is_string(), "{name}: Standard has sprite"),
                Mode::Transit => {
                    assert!(v.get("sprite").is_none(), "{name}: Transit omits sprite")
                }
            }
            // Round-trips through the serializer the step uses.
            let bytes = serde_json::to_vec_pretty(&v).unwrap();
            serde_json::from_slice::<Value>(&bytes).unwrap();
        }
    }

    /// Tile/glyph/sprite/overlay references use the literal `__BASE_URL__`.
    #[test]
    fn sources_reference_base_url_tiles_glyphs_sprite_overlays() {
        // Standard carries the sprite and the shield, no overlay sources.
        let std = render_style(
            "light",
            Mode::Standard,
            Theme::Light,
            "rome.pmtiles",
            &["metro-stations".to_string()],
        );
        assert_eq!(
            std["sources"]["openmaptiles"]["url"],
            "pmtiles://__BASE_URL__/tiles/rome.pmtiles"
        );
        assert_eq!(std["glyphs"], "__BASE_URL__/glyphs/{fontstack}/{range}.pbf");
        assert_eq!(std["sprite"], "__BASE_URL__/sprite/sprite");
        assert!(
            std["sources"]
                .as_object()
                .unwrap()
                .get("overlay-metro-stations")
                .is_none(),
            "standard styles do not wire overlay sources"
        );
        let ids = layer_ids(&std);
        assert!(ids.contains("road-shield"), "standard has the shield layer");

        // Transit omits the sprite + shield, wires the region's overlay sources.
        let transit = render_style(
            "transit-dark",
            Mode::Transit,
            Theme::Dark,
            "rome.pmtiles",
            &["metro-stations".to_string(), "transit-lines".to_string()],
        );
        assert!(transit.get("sprite").is_none(), "transit omits the sprite");
        assert_eq!(
            transit["sources"]["overlay-metro-stations"]["data"],
            "__BASE_URL__/overlays/metro-stations.geojson"
        );
        assert_eq!(
            transit["sources"]["overlay-transit-lines"]["data"],
            "__BASE_URL__/overlays/transit-lines.geojson"
        );
        let ids = layer_ids(&transit);
        assert!(!ids.contains("road-shield"), "transit has no shield layer");
    }

    fn layer_ids(style: &Value) -> HashSet<String> {
        style["layers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|l| l["id"].as_str().unwrap().to_string())
            .collect()
    }

    /// The on-disk bytes are byte-stable across two renders of the same inputs.
    #[test]
    fn render_is_byte_stable() {
        let a = render_style("light", Mode::Standard, Theme::Light, "rome.pmtiles", &[]);
        let b = render_style("light", Mode::Standard, Theme::Light, "rome.pmtiles", &[]);
        assert_eq!(
            serde_json::to_vec_pretty(&a).unwrap(),
            serde_json::to_vec_pretty(&b).unwrap()
        );
    }

    #[tokio::test]
    async fn run_writes_four_parsing_styles() {
        let dir = tempfile::tempdir().unwrap();
        RenderStyles.run(&ctx(dir.path())).await.unwrap();
        for (name, _, _) in STYLES {
            let path = dir.path().join(format!("output/styles/{name}.json"));
            let bytes = std::fs::read(&path).unwrap();
            assert!(bytes.ends_with(b"\n"), "{name}: trailing newline");
            let v: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(v["version"], 8);
            // The committed Rome region declares metro-stations; the tile source
            // is region-derived.
            assert_eq!(
                v["sources"]["openmaptiles"]["url"],
                "pmtiles://__BASE_URL__/tiles/rome.pmtiles"
            );
        }
        assert!(RenderStyles.satisfied(&ctx(dir.path())).await);
    }

    #[tokio::test]
    async fn satisfied_only_when_all_four_present_and_valid() {
        let dir = tempfile::tempdir().unwrap();
        let c = ctx(dir.path());
        assert!(!RenderStyles.satisfied(&c).await, "nothing rendered yet");

        RenderStyles.run(&c).await.unwrap();
        assert!(RenderStyles.satisfied(&c).await, "all four present");

        // A truncated/garbage style fails the parse guard → rebuild.
        std::fs::write(dir.path().join("output/styles/dark.json"), b"{ not json").unwrap();
        assert!(
            !RenderStyles.satisfied(&c).await,
            "invalid style is not satisfied"
        );

        // Missing file → not satisfied (fail-soft, no panic).
        std::fs::remove_file(dir.path().join("output/styles/light.json")).unwrap();
        assert!(!RenderStyles.satisfied(&c).await);
    }

    #[tokio::test]
    async fn rerun_reproduces_identical_bytes() {
        let dir = tempfile::tempdir().unwrap();
        let c = ctx(dir.path());
        RenderStyles.run(&c).await.unwrap();
        let first: Vec<u8> =
            std::fs::read(dir.path().join("output/styles/transit-light.json")).unwrap();
        // Second render of identical inputs: bytes must match (determinism). The
        // FORCE-overrides-satisfied gate itself is covered by the runner tests.
        RenderStyles.run(&c).await.unwrap();
        let second: Vec<u8> =
            std::fs::read(dir.path().join("output/styles/transit-light.json")).unwrap();
        assert_eq!(first, second, "re-render is byte-identical");
    }

    /// The rendered style names must equal the gateway/offline contract exactly,
    /// in order — otherwise the pipeline emits files the gateway 404s as unknown.
    #[test]
    fn style_names_match_contract_whitelist() {
        let names: Vec<&str> = STYLES.iter().map(|(n, _, _)| *n).collect();
        assert_eq!(
            names.as_slice(),
            iter_contracts::offline::STYLE_WHITELIST,
            "STYLES names must stay in lockstep with STYLE_WHITELIST"
        );
    }

    /// STYLES may only reference overlay GeoJSON sources OVERLAY actually builds.
    #[test]
    fn overlay_sources_are_a_subset_of_implemented() {
        let transit = render_style(
            "transit-light",
            Mode::Transit,
            Theme::Light,
            "rome.pmtiles",
            &["metro-stations".to_string(), "transit-lines".to_string()],
        );
        for key in transit["sources"].as_object().unwrap().keys() {
            if let Some(kind) = key.strip_prefix("overlay-") {
                assert!(
                    crate::steps::IMPLEMENTED_OVERLAY_KINDS.contains(&kind),
                    "wired overlay source {key} is not an implemented kind"
                );
            }
        }
    }

    /// A transit style with no overlays carries the tile source alone — no
    /// dangling `overlay-*` sources; one overlay yields exactly one.
    #[test]
    fn transit_overlay_source_count_tracks_input() {
        let none = render_style(
            "transit-light",
            Mode::Transit,
            Theme::Light,
            "rome.pmtiles",
            &[],
        );
        let sources = none["sources"].as_object().unwrap();
        assert_eq!(sources.len(), 1, "tile source alone with no overlays");
        assert!(
            !sources.keys().any(|k| k.starts_with("overlay-")),
            "no overlay-* sources for an empty overlay set"
        );

        let one = render_style(
            "transit-dark",
            Mode::Transit,
            Theme::Dark,
            "rome.pmtiles",
            &["metro-stations".to_string()],
        );
        let overlay_keys: Vec<&String> = one["sources"]
            .as_object()
            .unwrap()
            .keys()
            .filter(|k| k.starts_with("overlay-"))
            .collect();
        assert_eq!(overlay_keys, ["overlay-metro-stations"]);
    }

    /// A generated style survives the gateway's `__BASE_URL__` → host rewrite:
    /// the result re-parses as JSON and carries no leftover token. Keeps the
    /// generator honest about the literal the gateway substitutes.
    #[test]
    fn rendered_style_survives_base_url_rewrite() {
        let v = render_style(
            "transit-light",
            Mode::Transit,
            Theme::Light,
            "rome.pmtiles",
            &["metro-stations".to_string()],
        );
        let raw = serde_json::to_string(&v).unwrap();
        assert!(raw.contains("__BASE_URL__"), "style carries the token");
        // The same substitution the gateway performs per request.
        let rewritten = raw.replace("__BASE_URL__", "https://maps.test");
        let parsed: Value = serde_json::from_str(&rewritten).expect("rewrite stays valid JSON");
        assert!(
            !rewritten.contains("__BASE_URL__"),
            "no leftover token after rewrite"
        );
        assert_eq!(
            parsed["sources"]["openmaptiles"]["url"],
            "pmtiles://https://maps.test/tiles/rome.pmtiles"
        );
    }

    /// Object keys serialize sorted, which is what makes the styles byte-stable.
    /// Insert keys deliberately out of order and assert the bytes come back
    /// sorted: if a `serde_json` `preserve_order` feature flip ever switched maps
    /// to insertion order, this guard fails before the byte-stability claim
    /// silently breaks across versions.
    #[test]
    fn object_keys_serialize_sorted() {
        let mut m = serde_json::Map::new();
        m.insert("zebra".to_string(), json!(1));
        m.insert("alpha".to_string(), json!(2));
        m.insert("mike".to_string(), json!(3));
        let bytes = serde_json::to_string(&Value::Object(m)).unwrap();
        assert_eq!(
            bytes, r#"{"alpha":2,"mike":3,"zebra":1}"#,
            "serde_json must serialize object keys sorted (preserve_order off)"
        );
    }

    #[tokio::test]
    async fn missing_data_dir_is_created_not_panicking() {
        // A data dir that does not exist yet: run must create output/styles
        // rather than fail-hard.
        let dir = tempfile::tempdir().unwrap();
        let nested = dir.path().join("does/not/exist/yet");
        let c = Context::for_test(PathBuf::from(&nested), "test");
        RenderStyles.run(&c).await.unwrap();
        assert!(nested.join("output/styles/light.json").is_file());
    }
}
