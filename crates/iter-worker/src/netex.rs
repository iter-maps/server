//! Generic NeTEx → GTFS conversion (ADR 0017). The parser and the GTFS structure
//! are EU-standard and reusable for any country's NeTEx; the country-specific
//! bits — the id codespace scheme and the synthesized agency — go through the
//! [`iter_region_drivers::NetexProfile`] trait to a per-country driver (default:
//! Italian NeTEx-IT / Trenitalia-FL).
//!
//! The document is a single ~58 MB file, so we stream it with a pull parser and
//! resolve: `Line` → route, `ScheduledStopPoint` (with its `Location`) → stop,
//! `ServiceJourneyPattern` → the ordered stop sequence, `ServiceJourney` +
//! `passingTimes` → trip + stop_times (passing times reference a
//! `StopPointInJourneyPattern`, resolved back to its stop), and `DayType`
//! days-of-week + the journeys' `ValidBetween` → the calendar.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, Write};

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use iter_region_drivers::NetexProfile;

#[derive(Default)]
pub struct Netex {
    pub operator: String,
    pub lines: BTreeMap<String, Line>,
    pub stops: BTreeMap<String, Stop>,
    /// `StopPointInJourneyPattern` id → (sequence, stop id).
    pub pattern_pts: HashMap<String, (u32, String)>,
    /// `DayType` id → [mon, tue, …, sun].
    pub daytypes: BTreeMap<String, [bool; 7]>,
    pub journeys: Vec<Journey>,
    pub date_min: String,
    pub date_max: String,
}

pub struct Line {
    pub short: String,
    pub long: String,
    pub route_type: u8,
}
pub struct Stop {
    pub name: String,
    pub lat: String,
    pub lon: String,
}
pub struct Journey {
    pub id: String,
    pub line: String,
    pub service: String,
    pub headsign: String,
    pub times: Vec<PassTime>,
}
pub struct PassTime {
    pub sp: String,
    pub arr: String,
    pub dep: String,
}

/// Conversion counts, for the build-state report.
#[derive(Debug, Default, PartialEq)]
pub struct Stats {
    pub stops: usize,
    pub routes: usize,
    pub trips: usize,
    pub stop_times: usize,
    pub services: usize,
}

/// Stream-parse a NeTEx document into the intermediate model. Ids are stripped
/// through the supplied profile's codespace scheme.
pub fn parse<R: BufRead>(r: R, profile: &dyn NetexProfile) -> anyhow::Result<Netex> {
    let mut reader = Reader::from_reader(r);
    let mut buf = Vec::new();
    let mut nx = Netex::default();
    let gid = |s: &str| profile.strip_id(s);

    let mut stack: Vec<String> = Vec::new();
    let mut text = String::new();
    let mut line: Option<(String, Line)> = None;
    let mut stop: Option<(String, Stop)> = None;
    let mut daytype: Option<(String, [bool; 7])> = None;
    let mut journey: Option<Journey> = None;
    let mut in_operator = false;
    let mut in_pattern = false;
    let mut sp_pt: Option<(String, u32, String)> = None;
    let mut in_passing = false;
    let mut cur_time: Option<PassTime> = None;

    loop {
        match reader.read_event_into(&mut buf)? {
            Event::Eof => break,
            Event::Start(e) => {
                let name = local(e.name());
                match name.as_str() {
                    "Operator" => in_operator = true,
                    "Line" => {
                        line = Some((
                            gid(&attr(&e, "id")),
                            Line {
                                short: String::new(),
                                long: String::new(),
                                route_type: 3,
                            },
                        ));
                    }
                    "ScheduledStopPoint" => {
                        stop = Some((
                            gid(&attr(&e, "id")),
                            Stop {
                                name: String::new(),
                                lat: String::new(),
                                lon: String::new(),
                            },
                        ));
                    }
                    "DayType" => daytype = Some((gid(&attr(&e, "id")), [false; 7])),
                    "ServiceJourney" => {
                        journey = Some(Journey {
                            id: gid(&attr(&e, "id")),
                            line: String::new(),
                            service: String::new(),
                            headsign: String::new(),
                            times: Vec::new(),
                        });
                    }
                    "ServiceJourneyPattern" => in_pattern = true,
                    "StopPointInJourneyPattern" if in_pattern => {
                        sp_pt = Some((
                            gid(&attr(&e, "id")),
                            attr(&e, "order").parse().unwrap_or(0),
                            String::new(),
                        ));
                    }
                    "TimetabledPassingTime" if journey.is_some() => {
                        in_passing = true;
                        cur_time = Some(PassTime {
                            sp: String::new(),
                            arr: String::new(),
                            dep: String::new(),
                        });
                    }
                    _ => {}
                }
                stack.push(name);
                text.clear();
            }
            Event::Empty(e) => {
                let name = local(e.name());
                let r = || gid(&attr(&e, "ref"));
                match name.as_str() {
                    "ScheduledStopPointRef" => {
                        if let Some(sp) = sp_pt.as_mut() {
                            sp.2 = r();
                        }
                    }
                    "StopPointInJourneyPatternRef" => {
                        if let Some(t) = cur_time.as_mut() {
                            t.sp = r();
                        }
                    }
                    "DayTypeRef" => {
                        if let Some(j) = journey.as_mut()
                            && j.service.is_empty()
                        {
                            j.service = r();
                        }
                    }
                    "LineRef" => {
                        if let Some(j) = journey.as_mut()
                            && j.line.is_empty()
                        {
                            j.line = r();
                        }
                    }
                    _ => {}
                }
            }
            Event::Text(t) => text.push_str(&t.unescape().unwrap_or_default()),
            Event::End(e) => {
                let name = local(e.name());
                let parent = stack
                    .len()
                    .checked_sub(2)
                    .and_then(|i| stack.get(i))
                    .map(String::as_str)
                    .unwrap_or("");
                let txt = text.trim();

                match name.as_str() {
                    "Name" => {
                        if in_operator && parent == "Operator" {
                            nx.operator = txt.to_string();
                        } else if let Some((_, l)) = line.as_mut() {
                            if parent == "Line" {
                                l.long = txt.to_string();
                            }
                        } else if let Some((_, s)) = stop.as_mut() {
                            if parent == "ScheduledStopPoint" {
                                s.name = txt.to_string();
                            }
                        } else if let Some(j) = journey.as_mut() {
                            if parent == "ServiceJourney" {
                                j.headsign = txt.to_string();
                            }
                        }
                    }
                    "ShortName" => {
                        if let Some((_, l)) = line.as_mut()
                            && parent == "Line"
                        {
                            l.short = txt.to_string();
                        }
                    }
                    "TransportMode" => {
                        if let Some((_, l)) = line.as_mut()
                            && parent == "Line"
                        {
                            l.route_type = mode_to_type(txt);
                        }
                    }
                    "Longitude" => {
                        if let Some((_, s)) = stop.as_mut() {
                            s.lon = txt.to_string();
                        }
                    }
                    "Latitude" => {
                        if let Some((_, s)) = stop.as_mut() {
                            s.lat = txt.to_string();
                        }
                    }
                    "DaysOfWeek" => {
                        if let Some((_, d)) = daytype.as_mut() {
                            *d = parse_dow(txt);
                        }
                    }
                    "FromDate" if journey.is_some() => {
                        let d = date8(txt);
                        if !d.is_empty() && (nx.date_min.is_empty() || d < nx.date_min) {
                            nx.date_min = d;
                        }
                    }
                    "ToDate" if journey.is_some() => {
                        let d = date8(txt);
                        if d > nx.date_max {
                            nx.date_max = d;
                        }
                    }
                    "ArrivalTime" => {
                        if let Some(t) = cur_time.as_mut().filter(|_| in_passing) {
                            t.arr = txt.to_string();
                        }
                    }
                    "DepartureTime" => {
                        if let Some(t) = cur_time.as_mut().filter(|_| in_passing) {
                            t.dep = txt.to_string();
                        }
                    }
                    _ => {}
                }

                match name.as_str() {
                    "Operator" => in_operator = false,
                    "Line" => {
                        if let Some((id, l)) = line.take() {
                            nx.lines.insert(id, l);
                        }
                    }
                    "ScheduledStopPoint" => {
                        if let Some((id, s)) = stop.take() {
                            nx.stops.insert(id, s);
                        }
                    }
                    "DayType" => {
                        if let Some((id, d)) = daytype.take() {
                            nx.daytypes.insert(id, d);
                        }
                    }
                    "StopPointInJourneyPattern" => {
                        if let Some((id, order, sref)) = sp_pt.take()
                            && !sref.is_empty()
                        {
                            nx.pattern_pts.insert(id, (order, sref));
                        }
                    }
                    "ServiceJourneyPattern" => in_pattern = false,
                    "TimetabledPassingTime" => {
                        in_passing = false;
                        if let (Some(j), Some(t)) = (journey.as_mut(), cur_time.take())
                            && !t.sp.is_empty()
                        {
                            j.times.push(t);
                        }
                    }
                    "ServiceJourney" => {
                        if let Some(j) = journey.take() {
                            nx.journeys.push(j);
                        }
                    }
                    _ => {}
                }
                stack.pop();
                text.clear();
            }
            _ => {}
        }
        buf.clear();
    }
    Ok(nx)
}

/// Emit the parsed model as a GTFS feed (zip) and return the row counts. The
/// agency block and the route `agency_id` come from the supplied profile (ADR
/// 0017); the NeTEx feed's `Operator` name overrides the profile's agency name
/// when present.
pub fn write_gtfs_zip<W: Write + std::io::Seek>(
    nx: &Netex,
    profile: &dyn NetexProfile,
    w: W,
) -> anyhow::Result<Stats> {
    let mut zip = zip::ZipWriter::new(w);
    let opts: zip::write::SimpleFileOptions = zip::write::SimpleFileOptions::default();
    let info = profile.agency();
    let agency = if nx.operator.is_empty() {
        info.name
    } else {
        &nx.operator
    };
    let (start, end) = (nz(&nx.date_min, "20200101"), nz(&nx.date_max, "20201231"));
    let mut st = Stats::default();

    zip.start_file("agency.txt", opts)?;
    writeln!(
        zip,
        "agency_id,agency_name,agency_url,agency_timezone,agency_lang"
    )?;
    writeln!(
        zip,
        "{},{},{},{},{}",
        info.id,
        csv(agency),
        info.url,
        info.timezone,
        info.lang
    )?;

    zip.start_file("stops.txt", opts)?;
    writeln!(zip, "stop_id,stop_name,stop_lat,stop_lon")?;
    for (id, s) in &nx.stops {
        if s.lat.is_empty() || s.lon.is_empty() {
            continue;
        }
        writeln!(zip, "{},{},{},{}", csv(id), csv(&s.name), s.lat, s.lon)?;
        st.stops += 1;
    }

    zip.start_file("routes.txt", opts)?;
    writeln!(
        zip,
        "route_id,agency_id,route_short_name,route_long_name,route_type"
    )?;
    for (id, l) in &nx.lines {
        writeln!(
            zip,
            "{},{},{},{},{}",
            csv(id),
            info.id,
            csv(&l.short),
            csv(&l.long),
            l.route_type
        )?;
        st.routes += 1;
    }

    zip.start_file("calendar.txt", opts)?;
    writeln!(
        zip,
        "service_id,monday,tuesday,wednesday,thursday,friday,saturday,sunday,start_date,end_date"
    )?;
    for (id, d) in &nx.daytypes {
        writeln!(
            zip,
            "{},{},{},{},{},{},{},{},{start},{end}",
            csv(id),
            d[0] as u8,
            d[1] as u8,
            d[2] as u8,
            d[3] as u8,
            d[4] as u8,
            d[5] as u8,
            d[6] as u8,
        )?;
        st.services += 1;
    }

    zip.start_file("trips.txt", opts)?;
    writeln!(zip, "route_id,service_id,trip_id,trip_headsign")?;
    let mut valid_trips = HashMap::new();
    for j in &nx.journeys {
        if !nx.lines.contains_key(&j.line) || !nx.daytypes.contains_key(&j.service) {
            continue;
        }
        writeln!(
            zip,
            "{},{},{},{}",
            csv(&j.line),
            csv(&j.service),
            csv(&j.id),
            csv(&j.headsign)
        )?;
        valid_trips.insert(&j.id, ());
        st.trips += 1;
    }

    zip.start_file("stop_times.txt", opts)?;
    writeln!(
        zip,
        "trip_id,arrival_time,departure_time,stop_id,stop_sequence"
    )?;
    for j in &nx.journeys {
        if !valid_trips.contains_key(&j.id) {
            continue;
        }
        for t in &j.times {
            let Some((seq, stop_id)) = nx.pattern_pts.get(&t.sp) else {
                continue;
            };
            if !nx.stops.contains_key(stop_id) {
                continue;
            }
            let (arr, dep) = match (t.arr.is_empty(), t.dep.is_empty()) {
                (false, false) => (t.arr.as_str(), t.dep.as_str()),
                (true, false) => (t.dep.as_str(), t.dep.as_str()),
                (false, true) => (t.arr.as_str(), t.arr.as_str()),
                (true, true) => continue,
            };
            writeln!(zip, "{},{arr},{dep},{},{seq}", csv(&j.id), csv(stop_id))?;
            st.stop_times += 1;
        }
    }

    zip.finish()?;
    Ok(st)
}

fn local(name: quick_xml::name::QName) -> String {
    String::from_utf8_lossy(name.local_name().into_inner()).into_owned()
}
fn attr(e: &BytesStart, key: &str) -> String {
    e.attributes()
        .flatten()
        .find(|a| a.key.local_name().into_inner() == key.as_bytes())
        .map(|a| String::from_utf8_lossy(&a.value).into_owned())
        .unwrap_or_default()
}

fn mode_to_type(mode: &str) -> u8 {
    match mode {
        "tram" => 0,
        "metro" | "subway" => 1,
        "rail" => 2,
        "water" | "ferry" => 4,
        _ => 3,
    }
}

fn parse_dow(s: &str) -> [bool; 7] {
    let mut d = [false; 7];
    for tok in s.split_whitespace() {
        let i = match tok {
            "Monday" => 0,
            "Tuesday" => 1,
            "Wednesday" => 2,
            "Thursday" => 3,
            "Friday" => 4,
            "Saturday" => 5,
            "Sunday" => 6,
            _ => continue,
        };
        d[i] = true;
    }
    d
}

/// `2026-04-21T00:00:00.000+02:00` → `20260421`.
fn date8(s: &str) -> String {
    s.split('T').next().unwrap_or("").replace('-', "")
}

fn nz<'a>(s: &'a str, default: &'a str) -> &'a str {
    if s.is_empty() { default } else { s }
}

/// CSV-quote a field if it contains a comma, quote, or newline.
fn csv(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iter_region_drivers::{DEFAULT_NETEX_PROFILE, netex_profile};
    use std::io::Cursor;

    const SAMPLE: &str = r#"<PublicationDelivery>
      <ResourceFrame><organisations>
        <Operator id="IT:ITI4:Operator:1:0083:0"><Name>TRENITALIA</Name></Operator>
      </organisations></ResourceFrame>
      <ServiceFrame>
        <lines><Line id="IT:ITI4:Line:10083_pass_0083">
          <Name>Regionale</Name><ShortName>REG</ShortName><TransportMode>rail</TransportMode>
        </Line></lines>
        <scheduledStopPoints>
          <ScheduledStopPoint id="IT:ITI4:ScheduledStopPoint:A_0083">
            <Name>Roma</Name><Location><Longitude>12.5</Longitude><Latitude>41.9</Latitude></Location>
          </ScheduledStopPoint>
          <ScheduledStopPoint id="IT:ITI4:ScheduledStopPoint:B_0083">
            <Name>Cassino</Name><Location><Longitude>13.8</Longitude><Latitude>41.5</Latitude></Location>
          </ScheduledStopPoint>
        </scheduledStopPoints>
        <journeyPatterns><ServiceJourneyPattern id="IT:ITI4:ServiceJourneyPattern:P_0083">
          <Name>Roma - Cassino</Name>
          <pointsInSequence>
            <StopPointInJourneyPattern order="1" id="IT:ITI4:StopPointInJourneyPattern:P_0_0083">
              <ScheduledStopPointRef ref="IT:ITI4:ScheduledStopPoint:A_0083"/>
            </StopPointInJourneyPattern>
            <StopPointInJourneyPattern order="2" id="IT:ITI4:StopPointInJourneyPattern:P_1_0083">
              <ScheduledStopPointRef ref="IT:ITI4:ScheduledStopPoint:B_0083"/>
            </StopPointInJourneyPattern>
          </pointsInSequence>
        </ServiceJourneyPattern></journeyPatterns>
      </ServiceFrame>
      <ServiceCalendarFrame><dayTypes>
        <DayType id="IT:ITI4:DayType:0083_1"><Name>feriale</Name>
          <properties><PropertyOfDay><DaysOfWeek>Monday Tuesday Wednesday Thursday Friday</DaysOfWeek></PropertyOfDay></properties>
        </DayType>
      </dayTypes></ServiceCalendarFrame>
      <TimetableFrame><vehicleJourneys>
        <ServiceJourney id="IT:ITI4:ServiceJourney:J1_0083">
          <ValidBetween><FromDate>2026-04-21T00:00:00.000+02:00</FromDate><ToDate>2026-04-28T23:59:59.000+02:00</ToDate></ValidBetween>
          <Name>AVEZZANO - CASSINO</Name>
          <DepartureTime>13:05:00</DepartureTime>
          <dayTypes><DayTypeRef ref="IT:ITI4:DayType:0083_1"/></dayTypes>
          <FlexibleLineView><LineRef ref="IT:ITI4:Line:10083_pass_0083"/></FlexibleLineView>
          <passingTimes>
            <TimetabledPassingTime><StopPointInJourneyPatternRef ref="IT:ITI4:StopPointInJourneyPattern:P_0_0083"/><DepartureTime>13:05:00</DepartureTime></TimetabledPassingTime>
            <TimetabledPassingTime><StopPointInJourneyPatternRef ref="IT:ITI4:StopPointInJourneyPattern:P_1_0083"/><ArrivalTime>14:30:00</ArrivalTime><DepartureTime>14:30:00</DepartureTime></TimetabledPassingTime>
          </passingTimes>
        </ServiceJourney>
      </vehicleJourneys></TimetableFrame>
    </PublicationDelivery>"#;

    #[test]
    fn parses_the_netex_shape() {
        let profile = netex_profile(DEFAULT_NETEX_PROFILE);
        let nx = parse(Cursor::new(SAMPLE), profile.as_ref()).unwrap();
        assert_eq!(nx.operator, "TRENITALIA");
        assert_eq!(nx.lines.len(), 1);
        let line = nx.lines.get("10083_pass_0083").unwrap();
        assert_eq!(line.short, "REG");
        assert_eq!(line.route_type, 2); // rail
        assert_eq!(nx.stops.len(), 2);
        assert_eq!(nx.stops.get("A_0083").unwrap().name, "Roma");
        assert_eq!(nx.stops.get("A_0083").unwrap().lon, "12.5");
        assert_eq!(
            nx.pattern_pts.get("P_1_0083"),
            Some(&(2, "B_0083".to_string()))
        );
        assert_eq!(
            nx.daytypes.get("0083_1").unwrap(),
            &[true, true, true, true, true, false, false]
        );
        assert_eq!(nx.date_min, "20260421");
        assert_eq!(nx.date_max, "20260428");
        assert_eq!(nx.journeys.len(), 1);
        let j = &nx.journeys[0];
        assert_eq!(j.line, "10083_pass_0083");
        assert_eq!(j.service, "0083_1");
        assert_eq!(j.times.len(), 2);
    }

    #[test]
    fn emits_a_referentially_complete_gtfs() {
        let profile = netex_profile(DEFAULT_NETEX_PROFILE);
        let nx = parse(Cursor::new(SAMPLE), profile.as_ref()).unwrap();
        let mut out = Cursor::new(Vec::new());
        let st = write_gtfs_zip(&nx, profile.as_ref(), &mut out).unwrap();
        assert_eq!(st.routes, 1);
        assert_eq!(st.stops, 2);
        assert_eq!(st.trips, 1);
        assert_eq!(st.stop_times, 2);
        assert_eq!(st.services, 1);

        let mut zip = zip::ZipArchive::new(out).unwrap();
        for f in [
            "agency.txt",
            "stops.txt",
            "routes.txt",
            "trips.txt",
            "stop_times.txt",
            "calendar.txt",
        ] {
            assert!(zip.by_name(f).is_ok(), "{f} present");
        }
        // The profile's agency + the route's agency_id stay byte-for-byte the
        // Trenitalia-FL literals (the SAMPLE has an Operator, so the name is its
        // override).
        let mut agency = String::new();
        std::io::Read::read_to_string(&mut zip.by_name("agency.txt").unwrap(), &mut agency)
            .unwrap();
        assert!(agency.contains("FL,TRENITALIA,https://www.trenitalia.com,Europe/Rome,it"));
        let mut routes = String::new();
        std::io::Read::read_to_string(&mut zip.by_name("routes.txt").unwrap(), &mut routes)
            .unwrap();
        assert!(routes.contains("10083_pass_0083,FL,REG,Regionale,2"));

        let mut stop_times = String::new();
        std::io::Read::read_to_string(&mut zip.by_name("stop_times.txt").unwrap(), &mut stop_times)
            .unwrap();
        // the passing times resolved to ordered stops with times.
        assert!(stop_times.contains("J1_0083,13:05:00,13:05:00,A_0083,1"));
        assert!(stop_times.contains("J1_0083,14:30:00,14:30:00,B_0083,2"));
    }

    #[test]
    fn date8_and_csv_helpers() {
        // id stripping is the profile's job now (see iter-region-drivers).
        assert_eq!(date8("2026-04-21T00:00:00.000+02:00"), "20260421");
        assert_eq!(csv("A,B"), "\"A,B\"");
    }
}
