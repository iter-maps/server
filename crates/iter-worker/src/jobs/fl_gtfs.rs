//! FL regional-rail GTFS build. Trenitalia publishes no routable GTFS for the
//! FL lines, so the worker synthesizes one from the official NeTEx (the CCISS
//! NAP "RAP Lazio" feed) and writes it where the OTP graph build consumes it
//! (`<graph>/TRENITALIA-FL.gtfs.zip`). Runs on startup and daily. If `NETEX_URL`
//! is set the NeTEx is fetched; otherwise it's placed at `GATEWAY_NETEX_PATH`
//! (the NAP is login-gated, so auto-download needs a reachable URL).

use std::io::BufRead;
use std::path::{Path, PathBuf};
use std::time::Duration;

use async_trait::async_trait;
use tokio::io::AsyncWriteExt;

use crate::job::Job;
use crate::netex;
use crate::shapes::{self, RelInfo};

pub struct FlGtfsBuild {
    pub netex_path: PathBuf,
    pub out_path: PathBuf,
    pub netex_url: Option<String>,
    /// NeTEx profile id selecting the country driver (ADR 0017); the FL feed is
    /// the Italian NeTEx-IT default (`it-iti4`).
    pub netex_profile: String,
    /// Optional OSM clip to stitch `route=train` rail geometry into `shapes.txt`
    /// (extends ADR 0016). Absent/unreadable → GTFS is emitted without shapes,
    /// exactly as before (`OSM_CLIP_PATH`, default `<graph>/<region>.osm.pbf`).
    pub osm_clip_path: Option<PathBuf>,
    pub http: reqwest::Client,
}

#[async_trait]
impl Job for FlGtfsBuild {
    fn name(&self) -> &'static str {
        "fl-gtfs"
    }

    fn interval(&self) -> Duration {
        Duration::from_secs(24 * 60 * 60)
    }

    async fn run(&self) -> anyhow::Result<()> {
        // When a URL is set, refresh the NeTEx on every run (the daily cadence
        // is the refresh) — a failure is a warning, and we fall back to a
        // previously-placed/fetched file.
        if let Some(url) = &self.netex_url {
            tracing::info!(url, "fl-gtfs: fetching NeTEx");
            if let Err(e) = download(&self.http, url, &self.netex_path).await {
                tracing::warn!(error = %e, "fl-gtfs: NeTEx download failed; using existing file");
            }
        }

        if !exists(&self.netex_path).await {
            tracing::debug!(
                source = %self.netex_path.display(),
                "fl-gtfs: no NeTEx source present; nothing to build"
            );
            return Ok(());
        }

        let netex_path = self.netex_path.clone();
        let out = self.out_path.clone();
        let profile_id = self.netex_profile.clone();
        // Only stitch shapes when a clip is configured and present; otherwise the
        // build proceeds unchanged (shapes are a best-effort enrichment).
        let clip = self.osm_clip_path.clone().filter(|p| p.is_file());
        let stats = tokio::task::spawn_blocking(move || {
            convert_file(&netex_path, &out, &profile_id, clip.as_deref())
        })
        .await??;
        tracing::info!(
            stops = stats.stops,
            routes = stats.routes,
            trips = stats.trips,
            stop_times = stats.stop_times,
            services = stats.services,
            shapes = stats.shapes,
            out = %self.out_path.display(),
            "fl-gtfs: built GTFS from NeTEx"
        );
        Ok(())
    }
}

async fn exists(p: &Path) -> bool {
    tokio::fs::try_exists(p).await.unwrap_or(false)
}

async fn download(client: &reqwest::Client, url: &str, dest: &Path) -> anyhow::Result<()> {
    if let Some(parent) = dest.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let mut tmp = dest.to_path_buf().into_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);

    // Stream into the temp file; on any failure, clean it up so a partial
    // download never lingers (the NAP `checkedResource` streams slowly, so the
    // request gets a generous per-request timeout overriding the client's).
    let fetch = async {
        let mut resp = client
            .get(url)
            .timeout(Duration::from_secs(300))
            .send()
            .await?
            .error_for_status()?;
        let mut file = tokio::fs::File::create(&tmp).await?;
        while let Some(chunk) = resp.chunk().await? {
            file.write_all(&chunk).await?;
        }
        file.flush().await?;
        Ok::<(), anyhow::Error>(())
    };
    if let Err(e) = fetch.await {
        let _ = tokio::fs::remove_file(&tmp).await;
        return Err(e);
    }
    tokio::fs::rename(&tmp, dest).await?;
    Ok(())
}

/// Decompress (if `.gz`), parse the NeTEx, optionally stitch OSM-rail shapes, and
/// write the GTFS zip atomically. The profile id selects the country driver for
/// id stripping and the agency; `clip` (when present) is the OSM clip to stitch
/// `route=train` geometry from.
fn convert_file(
    netex: &Path,
    out: &Path,
    profile_id: &str,
    clip: Option<&Path>,
) -> anyhow::Result<netex::Stats> {
    let profile = iter_region_drivers::netex_profile(profile_id);
    let file = std::fs::File::open(netex)?;
    let reader: Box<dyn BufRead> = if netex.extension().and_then(|e| e.to_str()) == Some("gz") {
        Box::new(std::io::BufReader::new(flate2::read::GzDecoder::new(file)))
    } else {
        Box::new(std::io::BufReader::new(file))
    };
    let nx = netex::parse(reader, profile.as_ref())?;

    // Map an OSM line `ref` (e.g. "FL1") to the GTFS route id of the route whose
    // short name matches it, so a stitched relation's shape lands on the right
    // line. No clip → no shapes → GTFS unchanged.
    let stitched = clip.map(|c| stitch_shapes(c, &nx)).unwrap_or_default();

    if let Some(parent) = out.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut tmp = out.to_path_buf().into_os_string();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let stats = netex::write_gtfs_zip(
        &nx,
        profile.as_ref(),
        &stitched,
        std::fs::File::create(&tmp)?,
    )?;
    std::fs::rename(&tmp, out)?;
    Ok(stats)
}

/// Stitch the clip's `route=train` relations, labelling each shape with the GTFS
/// route id whose short name equals the relation's OSM `ref` (so trips on that
/// route point at the shape). Relations without a matching route are skipped.
fn stitch_shapes(clip: &Path, nx: &netex::Netex) -> Vec<shapes::Shape> {
    let by_short = short_to_route_id(nx);
    shapes::read_rail_shapes(clip, |rel: &RelInfo| route_for_rel(&by_short, rel))
}

/// Map each line's non-empty short name to its GTFS route id. Lines with an empty
/// short name are excluded so they can never match a relation.
fn short_to_route_id(nx: &netex::Netex) -> std::collections::HashMap<&str, &str> {
    nx.lines
        .iter()
        .filter(|(_, l)| !l.short.is_empty())
        .map(|(id, l)| (l.short.as_str(), id.as_str()))
        .collect()
}

/// Resolve a relation to a GTFS route id: match on the OSM `ref` (e.g. "FL1"),
/// falling back to the relation `name` for relations that carry the line only in
/// their name. `None` (no matching route) skips the relation.
fn route_for_rel(
    by_short: &std::collections::HashMap<&str, &str>,
    rel: &RelInfo,
) -> Option<String> {
    by_short
        .get(rel.route_ref.as_str())
        .or_else(|| by_short.get(rel.name.as_str()))
        .map(|id| id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use iter_region_drivers::DEFAULT_NETEX_PROFILE;

    fn job(netex_path: PathBuf, out_path: PathBuf) -> FlGtfsBuild {
        FlGtfsBuild {
            netex_path,
            out_path,
            netex_url: None,
            netex_profile: DEFAULT_NETEX_PROFILE.to_string(),
            osm_clip_path: None,
            http: reqwest::Client::new(),
        }
    }

    #[tokio::test]
    async fn skips_cleanly_when_no_source() {
        let dir = tempfile::tempdir().unwrap();
        let job = job(
            dir.path().join("absent.netex.xml.gz"),
            dir.path().join("out.gtfs.zip"),
        );
        assert!(job.run().await.is_ok());
        assert!(!job.out_path.exists(), "no output when no source");
    }

    #[tokio::test]
    async fn builds_gtfs_from_a_placed_netex() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("fl.netex.xml");
        std::fs::write(&src, SAMPLE).unwrap();
        let out = dir.path().join("TRENITALIA-FL.gtfs.zip");
        let job = job(src, out.clone());

        job.run().await.unwrap();
        assert!(out.is_file(), "GTFS zip written");

        let mut zip = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
        assert!(zip.by_name("stop_times.txt").is_ok());
        assert!(zip.by_name("calendar_dates.txt").is_ok());
        assert!(zip.by_name("calendar.txt").is_err());
    }

    #[test]
    fn route_for_rel_matches_ref_then_name_and_filters_empty_short() {
        // Two lines: "FL1" with a short name, plus one with an empty short name
        // that must never match. RelInfo carries the OSM ref and name tags.
        let mut nx = netex::Netex::default();
        nx.lines.insert(
            "route-fl1".to_string(),
            netex::Line {
                short: "FL1".to_string(),
                long: "Roma - Orte".to_string(),
                route_type: 2,
            },
        );
        nx.lines.insert(
            "route-blank".to_string(),
            netex::Line {
                short: String::new(),
                long: "Unnamed".to_string(),
                route_type: 2,
            },
        );
        let by_short = short_to_route_id(&nx);

        // ref match wins.
        let by_ref = RelInfo {
            route_ref: "FL1".to_string(),
            name: "irrelevant".to_string(),
        };
        assert_eq!(
            route_for_rel(&by_short, &by_ref).as_deref(),
            Some("route-fl1")
        );

        // empty ref → fall back to the name.
        let by_name = RelInfo {
            route_ref: String::new(),
            name: "FL1".to_string(),
        };
        assert_eq!(
            route_for_rel(&by_short, &by_name).as_deref(),
            Some("route-fl1")
        );

        // Neither ref nor name matches a route → relation skipped.
        let unknown = RelInfo {
            route_ref: "FL9".to_string(),
            name: "nope".to_string(),
        };
        assert_eq!(route_for_rel(&by_short, &unknown), None);

        // A line with an empty short name is never the match target.
        let empty = RelInfo {
            route_ref: String::new(),
            name: String::new(),
        };
        assert_eq!(route_for_rel(&by_short, &empty), None);
    }

    #[test]
    fn convert_file_without_a_clip_emits_the_legacy_feed() {
        // The fail-soft default at the conversion boundary: clip=None and a clip
        // pointing at a nonexistent file both produce the pre-shapes feed (no
        // shapes.txt, 4-column trips.txt header).
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("fl.netex.xml");
        std::fs::write(&src, SAMPLE).unwrap();

        for clip in [None, Some(Path::new("/no/such.pbf"))] {
            let out = dir.path().join("out.gtfs.zip");
            convert_file(&src, &out, DEFAULT_NETEX_PROFILE, clip).unwrap();
            let mut zip = zip::ZipArchive::new(std::fs::File::open(&out).unwrap()).unwrap();
            assert!(
                zip.by_name("shapes.txt").is_err(),
                "no shapes.txt: {clip:?}"
            );
            let mut trips = String::new();
            std::io::Read::read_to_string(&mut zip.by_name("trips.txt").unwrap(), &mut trips)
                .unwrap();
            assert_eq!(
                trips.lines().next().unwrap(),
                "route_id,service_id,trip_id,trip_headsign",
                "legacy header: {clip:?}"
            );
        }
    }

    #[test]
    fn schedule_metadata() {
        let job = job(PathBuf::from("/x"), PathBuf::from("/y"));
        assert_eq!(job.name(), "fl-gtfs");
        assert_eq!(job.interval().as_secs(), 24 * 60 * 60);
        assert!(job.run_on_start());
    }

    const SAMPLE: &str = r#"<PublicationDelivery>
      <ServiceFrame>
        <lines><Line id="IT:ITI4:Line:1_0083"><Name>Regionale</Name><ShortName>FL1</ShortName><TransportMode>rail</TransportMode></Line></lines>
        <scheduledStopPoints>
          <ScheduledStopPoint id="IT:ITI4:ScheduledStopPoint:A_0083"><Name>Roma</Name><Location><Longitude>12.5</Longitude><Latitude>41.9</Latitude></Location></ScheduledStopPoint>
          <ScheduledStopPoint id="IT:ITI4:ScheduledStopPoint:B_0083"><Name>Cassino</Name><Location><Longitude>13.8</Longitude><Latitude>41.5</Latitude></Location></ScheduledStopPoint>
        </scheduledStopPoints>
        <journeyPatterns><ServiceJourneyPattern id="IT:ITI4:ServiceJourneyPattern:P_0083"><pointsInSequence>
          <StopPointInJourneyPattern order="1" id="IT:ITI4:StopPointInJourneyPattern:P_0_0083"><ScheduledStopPointRef ref="IT:ITI4:ScheduledStopPoint:A_0083"/></StopPointInJourneyPattern>
          <StopPointInJourneyPattern order="2" id="IT:ITI4:StopPointInJourneyPattern:P_1_0083"><ScheduledStopPointRef ref="IT:ITI4:ScheduledStopPoint:B_0083"/></StopPointInJourneyPattern>
        </pointsInSequence></ServiceJourneyPattern></journeyPatterns>
      </ServiceFrame>
      <ServiceCalendarFrame><ServiceCalendar id="IT:ITI4:ServiceCalendar:0083">
        <dayTypes><DayType id="IT:ITI4:DayType:0083_1"><properties><PropertyOfDay><DaysOfWeek>Monday Tuesday Wednesday Thursday Friday</DaysOfWeek></PropertyOfDay></properties></DayType></dayTypes>
        <operatingPeriods><UicOperatingPeriod id="IT:ITI4:UicOperatingPeriod:0083_1"><FromDate>2026-04-21T00:00:00.000+02:00</FromDate><ToDate>2026-04-28T23:59:59.000+02:00</ToDate><ValidDayBits>11110011</ValidDayBits></UicOperatingPeriod></operatingPeriods>
        <dayTypeAssignments><DayTypeAssignment order="1" id="IT:ITI4:DayTypeAssignment:0083_1"><OperatingPeriodRef ref="IT:ITI4:UicOperatingPeriod:0083_1"/><DayTypeRef ref="IT:ITI4:DayType:0083_1"/></DayTypeAssignment></dayTypeAssignments>
      </ServiceCalendar></ServiceCalendarFrame>
      <TimetableFrame><vehicleJourneys><ServiceJourney id="IT:ITI4:ServiceJourney:J1_0083">
        <ValidBetween><FromDate>2026-04-21T00:00:00.000+02:00</FromDate><ToDate>2026-04-28T23:59:59.000+02:00</ToDate></ValidBetween>
        <Name>Roma - Cassino</Name>
        <dayTypes><DayTypeRef ref="IT:ITI4:DayType:0083_1"/></dayTypes>
        <FlexibleLineView><LineRef ref="IT:ITI4:Line:1_0083"/></FlexibleLineView>
        <passingTimes>
          <TimetabledPassingTime><StopPointInJourneyPatternRef ref="IT:ITI4:StopPointInJourneyPattern:P_0_0083"/><DepartureTime>13:05:00</DepartureTime></TimetabledPassingTime>
          <TimetabledPassingTime><StopPointInJourneyPatternRef ref="IT:ITI4:StopPointInJourneyPattern:P_1_0083"/><ArrivalTime>14:30:00</ArrivalTime><DepartureTime>14:30:00</DepartureTime></TimetabledPassingTime>
        </passingTimes>
      </ServiceJourney></vehicleJourneys></TimetableFrame>
    </PublicationDelivery>"#;
}
