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
//! `StopPointInJourneyPattern`, resolved back to its stop), and the calendar:
//! each `DayTypeAssignment` links a `DayType` to a `UicOperatingPeriod` whose
//! `ValidDayBits` are expanded into the exact running dates of `calendar_dates`
//! (ADR 0016). The `DaysOfWeek` are still read, but only to bound `date_min`/
//! `date_max`.

use std::collections::{BTreeMap, HashMap};
use std::io::{BufRead, Write};

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};

use iter_region_drivers::NetexProfile;

use crate::shapes::Shape;

#[derive(Default)]
pub struct Netex {
    pub operator: String,
    pub lines: BTreeMap<String, Line>,
    pub stops: BTreeMap<String, Stop>,
    /// `StopPointInJourneyPattern` id → (sequence, stop id).
    pub pattern_pts: HashMap<String, (u32, String)>,
    /// `DayType` id → [mon, tue, …, sun].
    pub daytypes: BTreeMap<String, [bool; 7]>,
    /// `UicOperatingPeriod` id → its from-date and `ValidDayBits`.
    pub operating_periods: BTreeMap<String, OperatingPeriod>,
    /// (`OperatingPeriodRef`, `DayTypeRef`) — each `DayTypeAssignment`.
    pub assignments: Vec<(String, String)>,
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
/// A `UicOperatingPeriod`: `bits[i]` = `1` means service runs on
/// `from` + `i` days (`from` is `YYYYMMDD`).
pub struct OperatingPeriod {
    pub from: String,
    pub bits: String,
}

/// Conversion counts, for the build-state report.
#[derive(Debug, Default, PartialEq)]
pub struct Stats {
    pub stops: usize,
    pub routes: usize,
    pub trips: usize,
    pub stop_times: usize,
    pub services: usize,
    /// Stitched OSM-rail shapes emitted (0 when no clip / no rail geometry).
    pub shapes: usize,
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
    // (id, from-date, bits) while inside a UicOperatingPeriod.
    let mut period: Option<(String, String, String)> = None;
    // (OperatingPeriodRef, DayTypeRef) while inside a DayTypeAssignment.
    let mut assignment: Option<(String, String)> = None;

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
                    "UicOperatingPeriod" => {
                        period = Some((gid(&attr(&e, "id")), String::new(), String::new()));
                    }
                    "DayTypeAssignment" => assignment = Some((String::new(), String::new())),
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
                    "OperatingPeriodRef" => {
                        if let Some(a) = assignment.as_mut() {
                            a.0 = r();
                        }
                    }
                    "DayTypeRef" => {
                        if let Some(a) = assignment.as_mut() {
                            a.1 = r();
                        } else if let Some(j) = journey.as_mut()
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
                    "FromDate" if period.is_some() => {
                        if let Some(p) = period.as_mut() {
                            p.1 = date8(txt);
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
                    "ValidDayBits" => {
                        if let Some(p) = period.as_mut() {
                            p.2 = txt.to_string();
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
                    "UicOperatingPeriod" => {
                        if let Some((id, from, bits)) = period.take()
                            && !from.is_empty()
                            && !bits.is_empty()
                        {
                            nx.operating_periods
                                .insert(id, OperatingPeriod { from, bits });
                        }
                    }
                    "DayTypeAssignment" => {
                        if let Some((op, dt)) = assignment.take()
                            && !op.is_empty()
                            && !dt.is_empty()
                        {
                            nx.assignments.push((op, dt));
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
    shapes: &[Shape],
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

    // Each service is the exact list of dates expanded from its
    // UicOperatingPeriod's ValidDayBits (calendar_dates-only, exception_type=1);
    // no calendar.txt is emitted. A service with zero dates is omitted so trips
    // referencing it are dropped too.
    let services = service_dates(nx);
    zip.start_file("calendar_dates.txt", opts)?;
    writeln!(zip, "service_id,date,exception_type")?;
    for (id, dates) in &services {
        for date in dates {
            writeln!(zip, "{},{date},1", csv(id))?;
        }
        st.services += 1;
    }

    // A trip's shape is the stitched OSM-rail polyline whose branch label matches
    // its route id; with no shapes (the default — no OSM clip), the column is
    // omitted so the output stays byte-for-byte what it was before shapes.
    //
    // Only branches we will actually emit as a shapes.txt row (>= 2 points) are
    // eligible: the shape_id we write on a trip must reference a real shape, so
    // the set here mirrors the emission guard below exactly. This keeps the
    // writer self-consistent for any caller-supplied slice, not just stitch()'s.
    let emitted: std::collections::HashSet<&str> = shapes
        .iter()
        .filter(|s| s.points.len() >= 2)
        .map(|s| s.branch.as_str())
        .collect();
    let with_shapes = !emitted.is_empty();

    zip.start_file("trips.txt", opts)?;
    if with_shapes {
        writeln!(zip, "route_id,service_id,trip_id,trip_headsign,shape_id")?;
    } else {
        writeln!(zip, "route_id,service_id,trip_id,trip_headsign")?;
    }
    let mut valid_trips = HashMap::new();
    for j in &nx.journeys {
        if !nx.lines.contains_key(&j.line) || !services.contains_key(&j.service) {
            continue;
        }
        if with_shapes {
            let shape = if emitted.contains(j.line.as_str()) {
                j.line.as_str()
            } else {
                ""
            };
            writeln!(
                zip,
                "{},{},{},{},{}",
                csv(&j.line),
                csv(&j.service),
                csv(&j.id),
                csv(&j.headsign),
                csv(shape)
            )?;
        } else {
            writeln!(
                zip,
                "{},{},{},{}",
                csv(&j.line),
                csv(&j.service),
                csv(&j.id),
                csv(&j.headsign)
            )?;
        }
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

    // shapes.txt only when stitched geometry exists; otherwise the feed is
    // exactly the no-shapes feed (OTP routes fine without it, ADR 0016).
    if with_shapes {
        zip.start_file("shapes.txt", opts)?;
        writeln!(zip, "shape_id,shape_pt_lat,shape_pt_lon,shape_pt_sequence")?;
        let mut written: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for s in shapes {
            // Skip degenerate shapes and any branch already emitted, so a GTFS
            // shape_id stays unique even if the caller passes duplicate branches.
            if s.points.len() < 2 || !written.insert(s.branch.as_str()) {
                continue;
            }
            for (seq, (lon, lat)) in s.points.iter().enumerate() {
                writeln!(zip, "{},{lat},{lon},{seq}", csv(&s.branch))?;
            }
            st.shapes += 1;
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

/// Resolve each `DayType` to its concrete running dates by joining the
/// `DayTypeAssignment`s to their `UicOperatingPeriod`s and expanding the
/// `ValidDayBits`. A service with no running dates is omitted.
fn service_dates(nx: &Netex) -> BTreeMap<String, Vec<String>> {
    let mut out: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (op_ref, dt_ref) in &nx.assignments {
        let Some(p) = nx.operating_periods.get(op_ref) else {
            continue;
        };
        let dates = expand_bits(&p.from, &p.bits);
        if !dates.is_empty() {
            out.entry(dt_ref.clone()).or_default().extend(dates);
        }
    }
    out
}

/// Expand `ValidDayBits` into `YYYYMMDD` dates: `bit[i]` (set) means service
/// runs on `from` + `i` days. `from` is `YYYYMMDD`; an unset bit (or
/// `isAvailable=false`, encoded as `0`) yields no date.
fn expand_bits(from: &str, bits: &str) -> Vec<String> {
    let Some(start) = days_from_ymd(from) else {
        return Vec::new();
    };
    bits.chars()
        .enumerate()
        .filter(|(_, c)| *c == '1')
        .map(|(i, _)| ymd_from_days(start + i as i64))
        .collect()
}

/// `YYYYMMDD` → days since 1970-01-01 (Howard Hinnant's `days_from_civil`).
fn days_from_ymd(s: &str) -> Option<i64> {
    if s.len() != 8 || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let y: i64 = s[0..4].parse().ok()?;
    let m: i64 = s[4..6].parse().ok()?;
    let d: i64 = s[6..8].parse().ok()?;
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = y - i64::from(m <= 2);
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146097 + doe - 719468)
}

/// Days since 1970-01-01 → `YYYYMMDD` (Howard Hinnant's `civil_from_days`).
fn ymd_from_days(z: i64) -> String {
    let z = z + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = y + i64::from(m <= 2);
    format!("{y:04}{m:02}{d:02}")
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
      <ServiceCalendarFrame><ServiceCalendar id="IT:ITI4:ServiceCalendar:0083">
        <dayTypes>
          <DayType id="IT:ITI4:DayType:0083_1"><Name>feriale</Name>
            <properties><PropertyOfDay><DaysOfWeek>Monday Tuesday Wednesday Thursday Friday</DaysOfWeek></PropertyOfDay></properties>
          </DayType>
        </dayTypes>
        <operatingPeriods>
          <UicOperatingPeriod id="IT:ITI4:UicOperatingPeriod:0083_1">
            <FromDate>2026-04-21T00:00:00.000+02:00</FromDate><ToDate>2026-04-28T23:59:59.000+02:00</ToDate>
            <ValidDayBits>11110011</ValidDayBits>
          </UicOperatingPeriod>
        </operatingPeriods>
        <dayTypeAssignments>
          <DayTypeAssignment order="1" id="IT:ITI4:DayTypeAssignment:0083_1">
            <OperatingPeriodRef ref="IT:ITI4:UicOperatingPeriod:0083_1"/>
            <DayTypeRef ref="IT:ITI4:DayType:0083_1"/>
          </DayTypeAssignment>
        </dayTypeAssignments>
      </ServiceCalendar></ServiceCalendarFrame>
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
        let op = nx.operating_periods.get("0083_1").unwrap();
        assert_eq!(op.from, "20260421");
        assert_eq!(op.bits, "11110011");
        assert_eq!(
            nx.assignments,
            vec![("0083_1".to_string(), "0083_1".to_string())]
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
        let st = write_gtfs_zip(&nx, profile.as_ref(), &[], &mut out).unwrap();
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
            "calendar_dates.txt",
        ] {
            assert!(zip.by_name(f).is_ok(), "{f} present");
        }
        // calendar_dates-only: no calendar.txt is emitted.
        assert!(zip.by_name("calendar.txt").is_err());

        // ValidDayBits 11110011 over 2026-04-21..28 → Sat/Sun (the 0 bits) off.
        let mut cal = String::new();
        std::io::Read::read_to_string(&mut zip.by_name("calendar_dates.txt").unwrap(), &mut cal)
            .unwrap();
        for date in [
            "20260421", "20260422", "20260423", "20260424", "20260427", "20260428",
        ] {
            assert!(cal.contains(&format!("0083_1,{date},1")), "{date} runs");
        }
        assert!(!cal.contains("0083_1,20260425,1")); // Sat off
        assert!(!cal.contains("0083_1,20260426,1")); // Sun off
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

    /// A minimal NeTEx with one DayType/UicOperatingPeriod/DayTypeAssignment for
    /// `0083_9` (and one journey on it), parameterized on from/to/bits.
    fn calendar_doc(from: &str, to: &str, bits: &str) -> String {
        format!(
            r#"<PublicationDelivery>
              <ServiceFrame><lines><Line id="IT:ITI4:Line:10083_pass_0083">
                <Name>Regionale</Name><ShortName>REG</ShortName><TransportMode>rail</TransportMode>
              </Line></lines></ServiceFrame>
              <ServiceCalendarFrame><ServiceCalendar id="IT:ITI4:ServiceCalendar:0083">
                <dayTypes>
                  <DayType id="IT:ITI4:DayType:0083_9"><Name>feriale ridotto</Name>
                    <properties><PropertyOfDay><DaysOfWeek>Monday Tuesday Wednesday Thursday</DaysOfWeek></PropertyOfDay></properties>
                  </DayType>
                </dayTypes>
                <operatingPeriods>
                  <UicOperatingPeriod id="IT:ITI4:UicOperatingPeriod:0083_9">
                    <FromDate>{from}T00:00:00.000+02:00</FromDate><ToDate>{to}T23:59:59.000+02:00</ToDate>
                    <ValidDayBits>{bits}</ValidDayBits>
                  </UicOperatingPeriod>
                </operatingPeriods>
                <dayTypeAssignments>
                  <DayTypeAssignment order="1" id="IT:ITI4:DayTypeAssignment:0083_9">
                    <OperatingPeriodRef ref="IT:ITI4:UicOperatingPeriod:0083_9"/>
                    <DayTypeRef ref="IT:ITI4:DayType:0083_9"/>
                  </DayTypeAssignment>
                </dayTypeAssignments>
              </ServiceCalendar></ServiceCalendarFrame>
              <TimetableFrame><vehicleJourneys>
                <ServiceJourney id="IT:ITI4:ServiceJourney:J9_0083">
                  <dayTypes><DayTypeRef ref="IT:ITI4:DayType:0083_9"/></dayTypes>
                  <FlexibleLineView><LineRef ref="IT:ITI4:Line:10083_pass_0083"/></FlexibleLineView>
                </ServiceJourney>
              </vehicleJourneys></TimetableFrame>
            </PublicationDelivery>"#
        )
    }

    fn calendar_dates_of(doc: &str) -> (Stats, String, bool) {
        let profile = netex_profile(DEFAULT_NETEX_PROFILE);
        let nx = parse(Cursor::new(doc), profile.as_ref()).unwrap();
        let mut out = Cursor::new(Vec::new());
        let st = write_gtfs_zip(&nx, profile.as_ref(), &[], &mut out).unwrap();
        let mut zip = zip::ZipArchive::new(out).unwrap();
        let mut cal = String::new();
        std::io::Read::read_to_string(&mut zip.by_name("calendar_dates.txt").unwrap(), &mut cal)
            .unwrap();
        let mut trips = String::new();
        std::io::Read::read_to_string(&mut zip.by_name("trips.txt").unwrap(), &mut trips).unwrap();
        let has_trip = trips.contains("J9_0083");
        (st, cal, has_trip)
    }

    #[test]
    fn valid_day_bits_excludes_a_weekday_holiday() {
        // 0083_9: DaysOfWeek is Mon-Thu, but bits exclude Fri Apr 24 *and* the
        // weekend — bits are authoritative, DaysOfWeek-over-a-span isn't.
        let (st, cal, has_trip) =
            calendar_dates_of(&calendar_doc("2026-04-21", "2026-04-28", "11100011"));
        assert!(has_trip);
        assert_eq!(st.services, 1);
        let rows: Vec<&str> = cal.lines().filter(|l| l.starts_with("0083_9,")).collect();
        assert_eq!(
            rows,
            vec![
                "0083_9,20260421,1",
                "0083_9,20260422,1",
                "0083_9,20260423,1",
                "0083_9,20260427,1",
                "0083_9,20260428,1",
            ]
        );
        // Apr 24 (Fri), 25 (Sat), 26 (Sun) are the 0 bits → no row.
        for off in ["20260424", "20260425", "20260426"] {
            assert!(!cal.contains(&format!("0083_9,{off},1")));
        }
    }

    #[test]
    fn single_day_window() {
        let (st, cal, _) = calendar_dates_of(&calendar_doc("2026-04-21", "2026-04-21", "1"));
        assert_eq!(st.services, 1);
        let rows: Vec<&str> = cal.lines().filter(|l| l.starts_with("0083_9,")).collect();
        assert_eq!(rows, vec!["0083_9,20260421,1"]);
    }

    #[test]
    fn all_zero_bits_drops_the_service_and_its_trips() {
        // No bit set (equivalently isAvailable=false for every day): the service
        // has zero dates, so it emits no rows and its trip is dropped.
        let (st, cal, has_trip) =
            calendar_dates_of(&calendar_doc("2026-04-21", "2026-04-28", "00000000"));
        assert_eq!(st.services, 0);
        assert!(!cal.contains("0083_9"));
        assert!(!has_trip);
    }

    #[test]
    fn shapes_emit_shapes_txt_and_wire_trip_shape_id() {
        let profile = netex_profile(DEFAULT_NETEX_PROFILE);
        let nx = parse(Cursor::new(SAMPLE), profile.as_ref()).unwrap();
        // One shape whose branch is the route id the SAMPLE's journey runs on.
        let shapes = vec![Shape {
            branch: "10083_pass_0083".to_string(),
            points: vec![(12.5, 41.9), (13.0, 41.7), (13.8, 41.5)],
        }];
        let mut out = Cursor::new(Vec::new());
        let st = write_gtfs_zip(&nx, profile.as_ref(), &shapes, &mut out).unwrap();
        assert_eq!(st.shapes, 1);

        let mut zip = zip::ZipArchive::new(out).unwrap();
        let mut shapes_txt = String::new();
        std::io::Read::read_to_string(&mut zip.by_name("shapes.txt").unwrap(), &mut shapes_txt)
            .unwrap();
        assert!(shapes_txt.starts_with("shape_id,shape_pt_lat,shape_pt_lon,shape_pt_sequence\n"));
        // lat,lon order (NOT lon,lat) and a 0-based sequence.
        assert!(shapes_txt.contains("10083_pass_0083,41.9,12.5,0"));
        assert!(shapes_txt.contains("10083_pass_0083,41.5,13.8,2"));

        // trips.txt gained the shape_id column and the journey references the shape.
        let mut trips = String::new();
        std::io::Read::read_to_string(&mut zip.by_name("trips.txt").unwrap(), &mut trips).unwrap();
        assert!(trips.lines().next().unwrap().ends_with(",shape_id"));
        assert!(trips.contains("J1_0083,AVEZZANO - CASSINO,10083_pass_0083"));
    }

    #[test]
    fn unmatched_route_gets_an_empty_shape_id() {
        let profile = netex_profile(DEFAULT_NETEX_PROFILE);
        let nx = parse(Cursor::new(SAMPLE), profile.as_ref()).unwrap();
        // A shape for a *different* branch — present (so the column exists) but the
        // SAMPLE journey's route has no shape, so its shape_id is blank.
        let shapes = vec![Shape {
            branch: "other_line".to_string(),
            points: vec![(1.0, 1.0), (2.0, 2.0)],
        }];
        let mut out = Cursor::new(Vec::new());
        write_gtfs_zip(&nx, profile.as_ref(), &shapes, &mut out).unwrap();
        let mut zip = zip::ZipArchive::new(out).unwrap();
        let mut trips = String::new();
        std::io::Read::read_to_string(&mut zip.by_name("trips.txt").unwrap(), &mut trips).unwrap();
        // Row ends with a trailing comma (empty shape_id), never a dangling ref.
        let row = trips.lines().find(|l| l.contains("J1_0083")).unwrap();
        assert!(
            row.ends_with(','),
            "unmatched route → empty shape_id: {row}"
        );
    }

    #[test]
    fn every_trip_shape_id_references_an_emitted_shape() {
        let profile = netex_profile(DEFAULT_NETEX_PROFILE);
        let nx = parse(Cursor::new(SAMPLE), profile.as_ref()).unwrap();
        // A single-point shape on the SAMPLE journey's route: shapes.txt skips it
        // (< 2 points), so the trip's shape_id MUST blank out rather than dangle.
        let shapes = vec![Shape {
            branch: "10083_pass_0083".to_string(),
            points: vec![(12.5, 41.9)],
        }];
        let mut out = Cursor::new(Vec::new());
        write_gtfs_zip(&nx, profile.as_ref(), &shapes, &mut out).unwrap();
        let mut zip = zip::ZipArchive::new(out).unwrap();

        // Collect shape_ids actually present in shapes.txt (if any).
        let mut emitted = std::collections::HashSet::new();
        if let Ok(mut f) = zip.by_name("shapes.txt") {
            let mut s = String::new();
            std::io::Read::read_to_string(&mut f, &mut s).unwrap();
            for row in s.lines().skip(1) {
                if let Some(id) = row.split(',').next() {
                    emitted.insert(id.to_string());
                }
            }
        }

        // Every non-empty shape_id referenced by trips.txt must have a shapes.txt row.
        let mut trips = String::new();
        std::io::Read::read_to_string(&mut zip.by_name("trips.txt").unwrap(), &mut trips).unwrap();
        let header: Vec<&str> = trips.lines().next().unwrap().split(',').collect();
        if let Some(col) = header.iter().position(|c| *c == "shape_id") {
            for row in trips.lines().skip(1) {
                let fields: Vec<&str> = row.split(',').collect();
                if let Some(id) = fields.get(col) {
                    if !id.is_empty() {
                        assert!(emitted.contains(*id), "dangling shape_id {id}");
                    }
                }
            }
        }
    }

    #[test]
    fn no_shapes_output_is_byte_identical_to_the_legacy_feed() {
        let profile = netex_profile(DEFAULT_NETEX_PROFILE);
        let nx = parse(Cursor::new(SAMPLE), profile.as_ref()).unwrap();

        // Empty shapes must reproduce the pre-shapes feed exactly: no shapes.txt,
        // and trips.txt with the original 4-column header (no shape_id).
        let mut out = Cursor::new(Vec::new());
        write_gtfs_zip(&nx, profile.as_ref(), &[], &mut out).unwrap();
        let mut zip = zip::ZipArchive::new(out).unwrap();
        assert!(
            zip.by_name("shapes.txt").is_err(),
            "no shapes.txt without a clip"
        );
        let mut trips = String::new();
        std::io::Read::read_to_string(&mut zip.by_name("trips.txt").unwrap(), &mut trips).unwrap();
        assert_eq!(
            trips.lines().next().unwrap(),
            "route_id,service_id,trip_id,trip_headsign",
            "legacy header unchanged"
        );
    }

    #[test]
    fn day_arithmetic_round_trips() {
        // Hinnant's algorithm: epoch, month/year boundaries, leap day.
        assert_eq!(days_from_ymd("19700101"), Some(0));
        assert_eq!(ymd_from_days(0), "19700101");
        assert_eq!(
            ymd_from_days(days_from_ymd("20260428").unwrap()),
            "20260428"
        );
        // Apr 21 + 6 days = Apr 27 (crosses no month boundary here).
        assert_eq!(
            ymd_from_days(days_from_ymd("20260421").unwrap() + 6),
            "20260427"
        );
        // Crossing a month boundary and a leap day.
        assert_eq!(
            ymd_from_days(days_from_ymd("20240228").unwrap() + 1),
            "20240229"
        );
        assert_eq!(
            ymd_from_days(days_from_ymd("20260131").unwrap() + 1),
            "20260201"
        );
    }
}
