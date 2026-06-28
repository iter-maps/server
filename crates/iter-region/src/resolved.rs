//! Resolving a target path (`italy/lazio/rome`) into one effective config by
//! walking the tree root→leaf and deep-merging each node.

use std::path::Path;

use anyhow::{Context, ensure};

use crate::profile::{Civici, Extents, Feed, Geocoding, LiveTrains, Overlay, Profile};

/// The merged effective config for a deployment target.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// The full target path, e.g. `italy/lazio/rome`.
    pub target: String,
    /// The leaf id, e.g. `rome`.
    pub id: String,
    /// The leaf's display name.
    pub name: String,
    pub extents: Extents,
    pub geocoding: Option<Geocoding>,
    pub live_trains: LiveTrains,
    pub civici: Civici,
    pub feeds: Vec<Feed>,
    pub overlays: Vec<Overlay>,
}

impl Resolved {
    /// Feeds that aren't explicitly disabled.
    pub fn enabled_feeds(&self) -> impl Iterator<Item = &Feed> {
        self.feeds.iter().filter(|f| f.is_enabled())
    }

    /// Scalars take the value closest to the target (last-wins); lists
    /// accumulate (a child re-declaring an id/kind overrides the ancestor's).
    fn merge(&mut self, p: Profile) {
        self.name = p.name;

        if p.extents.basemap.is_some() {
            self.extents.basemap = p.extents.basemap;
        }
        if p.extents.routing.is_some() {
            self.extents.routing = p.extents.routing;
        }
        if p.extents.overlay.is_some() {
            self.extents.overlay = p.extents.overlay;
        }
        if p.geocoding.is_some() {
            self.geocoding = p.geocoding;
        }
        if let Some(lt) = p.live_trains {
            if lt.provider.is_some() {
                self.live_trains.provider = lt.provider;
            }
            if lt.region_code.is_some() {
                self.live_trains.region_code = lt.region_code;
            }
        }
        if let Some(c) = p.civici {
            if c.bbox.is_some() {
                self.civici.bbox = c.bbox;
            }
            if c.enable.is_some() {
                self.civici.enable = c.enable;
            }
        }
        for feed in p.feeds {
            self.feeds.retain(|e| e.id != feed.id);
            self.feeds.push(feed);
        }
        for overlay in p.overlays {
            self.overlays.retain(|e| e.kind != overlay.kind);
            self.overlays.push(overlay);
        }
    }
}

/// Resolve a `/`-separated target against the region tree rooted at
/// `regions_dir`. Each path segment must be a directory carrying a
/// `region.toml`.
pub fn resolve(regions_dir: &Path, target: &str) -> anyhow::Result<Resolved> {
    let segments: Vec<&str> = target.split('/').filter(|s| !s.is_empty()).collect();
    ensure!(!segments.is_empty(), "empty region target");

    let mut resolved = Resolved {
        target: segments.join("/"),
        id: segments.last().unwrap().to_string(),
        name: String::new(),
        extents: Extents::default(),
        geocoding: None,
        live_trains: LiveTrains::default(),
        civici: Civici::default(),
        feeds: Vec::new(),
        overlays: Vec::new(),
    };

    let mut path = regions_dir.to_path_buf();
    for segment in &segments {
        path.push(segment);
        let toml_path = path.join("region.toml");
        let text = std::fs::read_to_string(&toml_path)
            .with_context(|| format!("region node not found: {}", toml_path.display()))?;
        let profile: Profile = toml::from_str(&text)
            .with_context(|| format!("invalid region profile: {}", toml_path.display()))?;
        resolved.merge(profile);
    }

    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    fn fixture_tree() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let w = |rel: &str, body: &str| {
            let p = dir.path().join(rel);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, body).unwrap();
        };
        w(
            "italy/region.toml",
            r#"
                name = "Italy"
                [extents]
                basemap = "6.6,35.3,18.6,47.1"
                [geocoding]
                photon_dump = "https://example/italy.jsonl.zst"
                [live_trains]
                provider = "viaggiatreno"
            "#,
        );
        w(
            "italy/lazio/region.toml",
            r#"
                name = "Lazio"
                [extents]
                routing = "11.3,41.1,14.05,43.35"
                [live_trains]
                region_code = 5
                [[feeds]]
                id = "COTRAL"
                url = "https://example/cotral.zip"
                insecure = true
                [[feeds]]
                id = "TRENITALIA-FL"
                source = "netex"
                optional = true
            "#,
        );
        w(
            "italy/lazio/rome/region.toml",
            r#"
                name = "Rome"
                [civici]
                bbox = "12.10,41.60,12.95,42.20"
                [[feeds]]
                id = "ATAC"
                url = "https://example/atac.zip"
                [[feeds.realtime]]
                channel = "trip-updates"
                url = "https://example/atac-tu.pb"
                [[overlays]]
                kind = "metro-stations"
            "#,
        );
        dir
    }

    #[test]
    fn resolves_leaf_merging_the_whole_chain() {
        let tree = fixture_tree();
        let r = resolve(tree.path(), "italy/lazio/rome").unwrap();

        assert_eq!(r.id, "rome");
        assert_eq!(r.name, "Rome");
        // basemap from the country root, routing from the region.
        assert_eq!(r.extents.basemap.as_deref(), Some("6.6,35.3,18.6,47.1"));
        assert_eq!(r.extents.routing.as_deref(), Some("11.3,41.1,14.05,43.35"));
        // civici from the city.
        assert_eq!(r.civici.bbox.as_deref(), Some("12.10,41.60,12.95,42.20"));
        // live-trains merged field-by-field across levels.
        assert_eq!(r.live_trains.provider.as_deref(), Some("viaggiatreno"));
        assert_eq!(r.live_trains.region_code, Some(5));
        assert!(r.geocoding.is_some());
        // feeds accumulate down the chain (service-area, not operator).
        let ids: Vec<_> = r.feeds.iter().map(|f| f.id.as_str()).collect();
        assert_eq!(ids, ["COTRAL", "TRENITALIA-FL", "ATAC"]);
        // the realtime channel carries its source URL down the chain.
        let atac = r.feeds.iter().find(|f| f.id == "ATAC").unwrap();
        assert_eq!(
            atac.realtime_url("trip-updates"),
            Some("https://example/atac-tu.pb")
        );
        assert_eq!(r.overlays.len(), 1);
    }

    #[test]
    fn resolving_an_ancestor_excludes_descendant_data() {
        let tree = fixture_tree();
        let r = resolve(tree.path(), "italy").unwrap();
        assert_eq!(r.name, "Italy");
        assert!(
            r.extents.routing.is_none(),
            "no transit clip at the country level"
        );
        assert!(
            r.feeds.is_empty(),
            "no regional/urban feeds at the country level"
        );
        assert!(r.geocoding.is_some());
    }

    #[test]
    fn unknown_node_errors() {
        let tree = fixture_tree();
        assert!(resolve(tree.path(), "italy/nope").is_err());
        assert!(resolve(tree.path(), "").is_err());
    }

    #[test]
    fn real_committed_profiles_parse_and_resolve() {
        let regions = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../regions");
        let r = resolve(&regions, "italy/lazio/rome").unwrap();
        assert_eq!(r.id, "rome");
        assert!(r.extents.basemap.is_some());
        assert!(r.extents.routing.is_some());
        let ids: Vec<_> = r.feeds.iter().map(|f| f.id.as_str()).collect();
        assert!(ids.contains(&"ATAC"));
        assert!(ids.contains(&"COTRAL"));
        assert!(ids.contains(&"TRENITALIA-FL"));
        assert_eq!(r.live_trains.region_code, Some(5));
        // the FL netex feed and the ATAC trip-updates channel carry their URLs.
        let fl = r.feeds.iter().find(|f| f.id == "TRENITALIA-FL").unwrap();
        assert_eq!(fl.source.as_deref(), Some("netex"));
        assert!(fl.url.is_some());
        let atac = r.feeds.iter().find(|f| f.id == "ATAC").unwrap();
        assert!(atac.realtime_url("trip-updates").is_some());
    }
}
