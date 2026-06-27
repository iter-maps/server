//! Place enrichment (ADR 0011, concept doc 20). Keyless fusion of open sources
//! into the normalized [`Place`]: Wikipedia REST summary (description, prose,
//! thumbnail, and the Wikidata QID for free), Wikidata for the structured image
//! (`P18`), and Wikimedia Commons for a proxiable thumbnail + its license/author.
//! Every displayed field carries source + license in `provenance`. Results are
//! TTL-cached + single-flighted (facts change slowly; this also shields the
//! rate-limited Wikimedia upstreams).
//!
//! `GET /places/enrich` returns one `Place`; `GET /places/image` proxies a
//! Commons file so the open-layer image is served through the BFF.

use std::time::Duration;

use axum::Json;
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::header;
use axum::response::Response;
use iter_contracts::places::{Category, Image, LonLat, Place, Provenance};
use iter_core::ApiError;
use serde::Deserialize;
use serde_json::Value;

use crate::http::{ApiErr, ApiResult};
use crate::state::AppState;

/// Place facts change slowly; cache hard (and keep Wikimedia happy).
const ENRICH_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const DEFAULT_IMAGE_WIDTH: u32 = 640;

#[derive(Debug, Deserialize, Default)]
pub struct EnrichParams {
    /// Wikidata QID (`Q10285`).
    pub wikidata: Option<String>,
    /// OSM-style `lang:Title` (`it:Colosseo`).
    pub wikipedia: Option<String>,
    /// Bare Wikipedia article title (needs `lang`).
    pub title: Option<String>,
    /// Output/lookup language; defaults to `it`.
    pub lang: Option<String>,
}

/// `GET /places/enrich` — resolve a seed (QID / wikipedia link / title) into one
/// enriched [`Place`].
pub async fn enrich(
    State(state): State<AppState>,
    Query(params): Query<EnrichParams>,
) -> ApiResult<Json<Place>> {
    let target = Target::from_params(&params)?;
    let key = target.cache_key();
    let place = state
        .places
        .get_or_fetch(&key, ENRICH_TTL, || build_place(&state, &target))
        .await?;
    Ok(Json(place))
}

#[derive(Debug, Deserialize)]
pub struct ImageParams {
    /// Commons file name, with or without the `File:` prefix.
    pub file: String,
    pub width: Option<u32>,
}

/// `GET /places/image` — proxy a Wikimedia Commons file through the BFF (the
/// open-layer image rule, concept 20 §5.1). We build the upstream URL from the
/// file name, so there is no open redirect / SSRF surface.
pub async fn image(
    State(state): State<AppState>,
    Query(params): Query<ImageParams>,
) -> ApiResult<Response> {
    let width = params.width.unwrap_or(DEFAULT_IMAGE_WIDTH).clamp(16, 2048);
    let url = commons_filepath_url(&strip_file_prefix(&params.file), width);
    let resp = state
        .http
        .get(&url)
        .header(header::USER_AGENT, user_agent(&state))
        .send()
        .await
        .map_err(|e| upstream(&e))?;
    if !resp.status().is_success() {
        return Err(ApiError::not_found("commons image not found").into());
    }
    let content_type = resp
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("image/jpeg")
        .to_owned();
    Response::builder()
        .header(header::CONTENT_TYPE, content_type)
        // CC images are immutable by file name; let clients cache hard.
        .header(header::CACHE_CONTROL, "public, max-age=604800")
        .body(Body::from_stream(resp.bytes_stream()))
        .map_err(|_| ApiErr::from(ApiError::internal("failed to build image response")))
}

/// What the request resolved to: a language + Wikipedia title, optionally a QID.
struct Target {
    lang: String,
    title: Option<String>,
    qid: Option<String>,
}

impl Target {
    fn from_params(p: &EnrichParams) -> Result<Self, ApiErr> {
        let lang = p.lang.clone().unwrap_or_else(|| "it".to_string());
        if let Some(wp) = &p.wikipedia {
            // `lang:Title`
            let (l, t) = wp
                .split_once(':')
                .ok_or_else(|| ApiError::bad_request("wikipedia must be 'lang:Title'"))?;
            return Ok(Self {
                lang: l.to_string(),
                title: Some(t.to_string()),
                qid: None,
            });
        }
        if let Some(t) = &p.title {
            return Ok(Self {
                lang,
                title: Some(t.clone()),
                qid: None,
            });
        }
        if let Some(q) = &p.wikidata {
            return Ok(Self {
                lang,
                title: None,
                qid: Some(q.clone()),
            });
        }
        Err(ApiError::bad_request("provide one of: wikidata, wikipedia, title").into())
    }

    fn cache_key(&self) -> String {
        format!(
            "{}|{}|{}",
            self.lang,
            self.title.as_deref().unwrap_or(""),
            self.qid.as_deref().unwrap_or("")
        )
    }
}

async fn build_place(state: &AppState, target: &Target) -> ApiResult<Place> {
    // Resolve a (lang, title) — from a QID's sitelink if we only have the QID.
    let (lang, title, mut qid) = match (&target.title, &target.qid) {
        (Some(t), q) => (target.lang.clone(), t.clone(), q.clone()),
        (None, Some(q)) => {
            let entity = fetch_entity(state, q).await?;
            let (l, t) = wikidata_sitelink(&entity, q, &target.lang)
                .ok_or_else(|| ApiError::not_found("no Wikipedia article for that QID"))?;
            (l, t, Some(q.clone()))
        }
        (None, None) => return Err(ApiError::bad_request("nothing to resolve").into()),
    };

    let summary = fetch_summary(state, &lang, &title).await?;
    let s = parse_summary(&summary);
    qid = s.qid.clone().or(qid);

    let location = LonLat {
        lon: s
            .lon
            .ok_or_else(|| ApiError::not_found("no coordinates for place"))?,
        lat: s.lat.unwrap_or_default(),
    };

    let mut provenance = Vec::new();
    if s.description.is_some() {
        provenance.push(Provenance {
            field: "description".into(),
            source: "wikidata".into(),
            license: Some("CC0".into()),
            url: None,
        });
    }
    if s.summary.is_some() {
        provenance.push(Provenance {
            field: "summary".into(),
            source: "wikipedia".into(),
            license: Some("CC-BY-SA".into()),
            url: s.article_url.clone(),
        });
    }

    // Image: prefer Wikidata P18 (a Commons file we can proxy + attribute);
    // fall back to the summary thumbnail (already a CDN URL).
    let mut image = None;
    if let Some(q) = &qid
        && let Ok(entity) = fetch_entity(state, q).await
        && let Some(file) = wikidata_claim_string(&entity, q, "P18")
    {
        let meta = fetch_image_meta(state, &file).await.ok().flatten();
        image = Some(proxied_image(&file, meta.as_ref()));
        provenance.push(Provenance {
            field: "image".into(),
            source: "wikimedia-commons".into(),
            license: meta.and_then(|m| m.license),
            url: Some(commons_file_page(&file)),
        });
    }
    if image.is_none()
        && let Some(thumb) = s.thumb.clone()
    {
        image = Some(Image {
            url: thumb,
            license: None,
            author: None,
            attribution: None,
            proxied: false,
        });
    }

    let mut names = std::collections::BTreeMap::new();
    names.insert(lang.clone(), s.title.clone());

    Ok(Place {
        id: qid
            .map(|q| format!("wd:{q}"))
            .unwrap_or_else(|| format!("wp:{lang}:{title}")),
        name: s.title,
        names,
        category: Category {
            primary: "place".into(),
            tags: vec![],
        },
        location,
        description: s.description,
        summary: s.summary,
        image,
        address: None,
        facets: Default::default(),
        related: vec![],
        provenance,
    })
}

// ─── Upstream fetches ────────────────────────────────────────────────────────

fn user_agent(state: &AppState) -> String {
    // Wikimedia enforces a contact-bearing User-Agent (else lowest rate tier).
    format!(
        "iter-maps/{} (+https://github.com/iter-maps/server)",
        state.cfg.version
    )
}

async fn fetch_json(state: &AppState, url: &str) -> ApiResult<Value> {
    let resp = state
        .http
        .get(url)
        .header(header::USER_AGENT, user_agent(state))
        .send()
        .await
        .map_err(|e| upstream(&e))?;
    if resp.status() == axum::http::StatusCode::NOT_FOUND {
        return Err(ApiError::not_found("upstream returned 404").into());
    }
    if !resp.status().is_success() {
        return Err(ApiError::upstream_unavailable("wikimedia upstream error").into());
    }
    resp.json::<Value>()
        .await
        .map_err(|_| ApiErr::from(ApiError::upstream_unavailable("malformed upstream JSON")))
}

async fn fetch_summary(state: &AppState, lang: &str, title: &str) -> ApiResult<Value> {
    let url = format!(
        "https://{lang}.wikipedia.org/api/rest_v1/page/summary/{}",
        pct(title)
    );
    fetch_json(state, &url).await
}

async fn fetch_entity(state: &AppState, qid: &str) -> ApiResult<Value> {
    let url = format!("https://www.wikidata.org/wiki/Special:EntityData/{qid}.json");
    fetch_json(state, &url).await
}

async fn fetch_image_meta(state: &AppState, file: &str) -> ApiResult<Option<ImageMeta>> {
    let url = format!(
        "https://commons.wikimedia.org/w/api.php?action=query&format=json&prop=imageinfo&iiprop=extmetadata&titles=File:{}",
        pct(file)
    );
    let v = fetch_json(state, &url).await?;
    Ok(parse_image_meta(&v))
}

fn upstream(e: &reqwest::Error) -> ApiErr {
    if e.is_timeout() {
        ApiError::timeout("wikimedia request timed out").into()
    } else {
        ApiError::upstream_unavailable("wikimedia is unavailable").into()
    }
}

// ─── Pure parsers (unit-tested) ──────────────────────────────────────────────

struct Summary {
    title: String,
    description: Option<String>,
    summary: Option<String>,
    qid: Option<String>,
    thumb: Option<String>,
    article_url: Option<String>,
    lon: Option<f64>,
    lat: Option<f64>,
}

fn parse_summary(v: &Value) -> Summary {
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(str::to_string);
    Summary {
        // `title` is plain text; `displaytitle` is HTML (strip it as a fallback).
        title: s("title")
            .or_else(|| s("displaytitle").map(|d| strip_html(&d)))
            .unwrap_or_default(),
        description: s("description"),
        summary: v
            .get("extract")
            .and_then(Value::as_str)
            .and_then(|e| (!e.trim().is_empty()).then(|| e.to_string())),
        qid: s("wikibase_item"),
        thumb: v
            .get("thumbnail")
            .and_then(|t| t.get("source"))
            .and_then(Value::as_str)
            .map(str::to_string),
        article_url: v
            .get("content_urls")
            .and_then(|c| c.get("desktop"))
            .and_then(|d| d.get("page"))
            .and_then(Value::as_str)
            .map(str::to_string),
        lon: v
            .get("coordinates")
            .and_then(|c| c.get("lon"))
            .and_then(Value::as_f64),
        lat: v
            .get("coordinates")
            .and_then(|c| c.get("lat"))
            .and_then(Value::as_f64),
    }
}

/// A claim whose value is a plain string (P18 image filename, P856 url).
fn wikidata_claim_string(entity: &Value, qid: &str, prop: &str) -> Option<String> {
    entity
        .get("entities")?
        .get(qid)?
        .get("claims")?
        .get(prop)?
        .as_array()?
        .first()?
        .get("mainsnak")?
        .get("datavalue")?
        .get("value")?
        .as_str()
        .map(str::to_string)
}

/// The Wikipedia sitelink for a QID, preferring `{lang}wiki`, then `enwiki`.
fn wikidata_sitelink(entity: &Value, qid: &str, lang: &str) -> Option<(String, String)> {
    let sitelinks = entity.get("entities")?.get(qid)?.get("sitelinks")?;
    for l in [lang, "en"] {
        if let Some(t) = sitelinks
            .get(format!("{l}wiki"))
            .and_then(|s| s.get("title"))
            .and_then(Value::as_str)
        {
            return Some((l.to_string(), t.to_string()));
        }
    }
    None
}

struct ImageMeta {
    license: Option<String>,
    author: Option<String>,
    attribution: Option<String>,
}

fn parse_image_meta(v: &Value) -> Option<ImageMeta> {
    let pages = v.get("query")?.get("pages")?.as_object()?;
    let info = pages
        .values()
        .next()?
        .get("imageinfo")?
        .as_array()?
        .first()?
        .get("extmetadata")?;
    let field = |k: &str| {
        info.get(k)
            .and_then(|f| f.get("value"))
            .and_then(Value::as_str)
            .map(strip_html)
    };
    Some(ImageMeta {
        license: field("LicenseShortName"),
        author: field("Artist"),
        attribution: field("Attribution").or_else(|| field("Credit")),
    })
}

fn proxied_image(file: &str, meta: Option<&ImageMeta>) -> Image {
    Image {
        url: format!(
            "/places/image?file={}&width={DEFAULT_IMAGE_WIDTH}",
            pct(file)
        ),
        license: meta.and_then(|m| m.license.clone()),
        author: meta.and_then(|m| m.author.clone()),
        attribution: meta.and_then(|m| m.attribution.clone()),
        proxied: true,
    }
}

fn commons_filepath_url(file: &str, width: u32) -> String {
    format!(
        "https://commons.wikimedia.org/wiki/Special:FilePath/{}?width={width}",
        pct(file)
    )
}

fn commons_file_page(file: &str) -> String {
    format!("https://commons.wikimedia.org/wiki/File:{}", pct(file))
}

fn strip_file_prefix(file: &str) -> String {
    file.strip_prefix("File:").unwrap_or(file).to_string()
}

/// Percent-encode a path/query value (titles carry spaces, accents, apostrophes).
fn pct(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Strip the HTML the Commons `Artist`/`Attribution` fields wrap their value in.
fn strip_html(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut in_tag = false;
    for c in s.chars() {
        match c {
            '<' => in_tag = true,
            '>' => in_tag = false,
            _ if !in_tag => out.push(c),
            _ => {}
        }
    }
    out.trim().to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_wikipedia_summary() {
        let v = json!({
            "title": "Colosseo",
            "displaytitle": "<span class=\"mw-page-title-main\">Colosseo</span>",
            "description": "anfiteatro romano",
            "extract": "Il Colosseo è un anfiteatro.",
            "wikibase_item": "Q10285",
            "thumbnail": { "source": "https://upload.wikimedia.org/thumb.jpg" },
            "content_urls": { "desktop": { "page": "https://it.wikipedia.org/wiki/Colosseo" } },
            "coordinates": { "lat": 41.8902, "lon": 12.4922 }
        });
        let s = parse_summary(&v);
        assert_eq!(s.title, "Colosseo");
        assert_eq!(s.description.as_deref(), Some("anfiteatro romano"));
        assert_eq!(s.qid.as_deref(), Some("Q10285"));
        assert_eq!(
            s.thumb.as_deref(),
            Some("https://upload.wikimedia.org/thumb.jpg")
        );
        assert_eq!(s.lon, Some(12.4922));
        assert_eq!(s.lat, Some(41.8902));
        assert!(s.summary.unwrap().contains("anfiteatro"));
    }

    #[test]
    fn empty_extract_is_treated_as_absent() {
        let s = parse_summary(&json!({ "title": "X", "extract": "   " }));
        assert!(s.summary.is_none());
    }

    #[test]
    fn extracts_p18_and_sitelink() {
        let entity = json!({
            "entities": { "Q10285": {
                "claims": { "P18": [ { "mainsnak": { "datavalue": { "value": "Colosseo 2020.jpg" } } } ] },
                "sitelinks": { "itwiki": { "title": "Colosseo" }, "enwiki": { "title": "Colosseum" } }
            } }
        });
        assert_eq!(
            wikidata_claim_string(&entity, "Q10285", "P18").as_deref(),
            Some("Colosseo 2020.jpg")
        );
        assert_eq!(
            wikidata_sitelink(&entity, "Q10285", "it"),
            Some(("it".to_string(), "Colosseo".to_string()))
        );
        // falls back to enwiki when the requested lang is absent.
        assert_eq!(
            wikidata_sitelink(&entity, "Q10285", "fr"),
            Some(("en".to_string(), "Colosseum".to_string()))
        );
    }

    #[test]
    fn parses_commons_extmetadata() {
        let v = json!({ "query": { "pages": { "123": { "imageinfo": [ { "extmetadata": {
            "LicenseShortName": { "value": "CC BY-SA 4.0" },
            "Artist": { "value": "<a href=\"x\">User:FeaturedPics</a>" }
        } } ] } } } });
        let m = parse_image_meta(&v).unwrap();
        assert_eq!(m.license.as_deref(), Some("CC BY-SA 4.0"));
        assert_eq!(m.author.as_deref(), Some("User:FeaturedPics"));
    }

    #[test]
    fn percent_encodes_titles_and_files() {
        assert_eq!(pct("Colosseo 2020.jpg"), "Colosseo%202020.jpg");
        assert_eq!(pct("Sant'Angelo"), "Sant%27Angelo");
        assert!(commons_filepath_url("A B.jpg", 320).ends_with("A%20B.jpg?width=320"));
    }

    #[test]
    fn proxied_image_points_at_the_gateway() {
        let img = proxied_image("Colosseo 2020.jpg", None);
        assert!(
            img.url
                .starts_with("/places/image?file=Colosseo%202020.jpg")
        );
        assert!(img.proxied);
    }

    #[test]
    fn strips_file_prefix_and_html() {
        assert_eq!(strip_file_prefix("File:X.jpg"), "X.jpg");
        assert_eq!(strip_file_prefix("X.jpg"), "X.jpg");
        assert_eq!(strip_html("<a href=\"y\">Jane Doe</a>"), "Jane Doe");
    }

    #[test]
    fn target_resolution_from_params() {
        let t = Target::from_params(&EnrichParams {
            wikipedia: Some("it:Colosseo".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(t.lang, "it");
        assert_eq!(t.title.as_deref(), Some("Colosseo"));

        let t = Target::from_params(&EnrichParams {
            wikidata: Some("Q10285".into()),
            ..Default::default()
        })
        .unwrap();
        assert_eq!(t.qid.as_deref(), Some("Q10285"));
        assert!(t.title.is_none());

        // nothing to resolve → bad request.
        assert!(Target::from_params(&EnrichParams::default()).is_err());
    }
}
