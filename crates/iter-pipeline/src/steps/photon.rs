//! PHOTON — build the geocoding search index from the region's OSM-derived
//! Photon dump plus the civici house docs the CIVICI step emits. Photon 1.x
//! embeds OpenSearch and imports a JSON dump with no Postgres: we decompress
//! the dump, append civici (with the load-bearing trailing-newline fix so the
//! last dump doc and the first civico don't glue together), and run
//! `photon.jar import`. `-extra-tags wikidata,wikipedia,wikimedia_commons`
//! keeps those tags queryable so the enrichment layer can reach images.
//!
//! Import is all-or-nothing: the index dir is wiped first, so a half-failed
//! prior import can't contaminate the next. Skip-if-present; `FORCE_PHOTON`
//! reimports. No-ops for a region without geocoding.

use std::path::Path;

use async_trait::async_trait;
use iter_core::config;

use crate::context::Context;
use crate::fsx;
use crate::step::Step;

pub struct BuildPhotonIndex;

#[async_trait]
impl Step for BuildPhotonIndex {
    fn name(&self) -> &'static str {
        "PHOTON"
    }

    async fn satisfied(&self, ctx: &Context) -> bool {
        index_present(&ctx.photon_dir()).await
    }

    async fn run(&self, ctx: &Context) -> anyhow::Result<()> {
        let Some(geo) = ctx.region.geocoding.as_ref() else {
            tracing::info!("region has no geocoding; skipping photon index");
            return Ok(());
        };

        let url = config::or("PHOTON_DUMP_URL", &geo.photon_dump);
        let cc = config::or("PHOTON_COUNTRY_CODES", &geo.country_codes);
        let langs = config::or("PHOTON_LANGUAGES", &geo.languages);
        let heap = config::or("PHOTON_IMPORT_HEAP", "2g");
        let jar = config::or("PHOTON_JAR", "/opt/photon.jar");

        let photon_dir = ctx.photon_dir();
        tokio::fs::create_dir_all(&photon_dir).await?;

        // 1. Fetch the dump (skip if cached; FORCE_PHOTON re-fetches).
        let dump = ctx.output("sources/photon-dump.jsonl.zst");
        if !dump.is_file() || ctx.forced("PHOTON") {
            fsx::download(&ctx.http, &url, &dump).await?;
        }

        // 2. Decompress to the import file, then append the civici house docs.
        let import_file = photon_dir.join("import.jsonl");
        decompress(&dump, &import_file).await?;
        let civici = photon_dir.join("civici.jsonl");
        if civici.is_file() {
            append_with_newline(&import_file, &civici).await?;
            tracing::info!("appended civici house docs to the import stream");
        }

        // 3. All-or-nothing: wipe any prior index before reimport.
        let data = photon_dir.join("photon_data");
        if data.exists() {
            tokio::fs::remove_dir_all(&data).await?;
        }

        // 4. Import (builds photon_data from scratch; embedded OpenSearch).
        let args = photon_import_args(&heap, &jar, &import_file, &photon_dir, &cc, &langs);
        tracing::info!(?args, "importing photon index");
        let status = tokio::process::Command::new("java")
            .args(&args)
            .status()
            .await?;
        anyhow::ensure!(status.success(), "photon import exited with {status}");
        anyhow::ensure!(
            index_present(&photon_dir).await,
            "photon import reported success but the index is empty"
        );

        // The decompressed import file is large and regenerable; drop it.
        let _ = tokio::fs::remove_file(&import_file).await;
        Ok(())
    }
}

/// The Photon index exists once `photon_data` holds entries. Photon 1.x writes
/// `photon_data/node_1` (OpenSearch); pre-1.0 wrote `photon_data/elasticsearch`
/// — accept any non-empty `photon_data` for back-compat.
async fn index_present(photon_dir: &Path) -> bool {
    let data = photon_dir.join("photon_data");
    let Ok(mut rd) = tokio::fs::read_dir(&data).await else {
        return false;
    };
    matches!(rd.next_entry().await, Ok(Some(_)))
}

async fn decompress(src: &Path, dest: &Path) -> anyhow::Result<()> {
    let status = tokio::process::Command::new("zstd")
        .args(["-d", "-f", "-o"])
        .arg(dest)
        .arg(src)
        .status()
        .await?;
    anyhow::ensure!(status.success(), "zstd exited with {status}");
    Ok(())
}

/// Append `extra` to `base`, ensuring `base` ends with a newline first — Photon
/// reads one JSON document per line, so a missing terminator would merge the
/// dump's last doc with the first civico and corrupt the import.
async fn append_with_newline(base: &Path, extra: &Path) -> anyhow::Result<()> {
    use tokio::io::AsyncWriteExt;

    let needs_nl = match tokio::fs::read(base).await {
        Ok(bytes) => bytes.last() != Some(&b'\n'),
        Err(_) => false,
    };
    let mut f = tokio::fs::OpenOptions::new()
        .append(true)
        .open(base)
        .await?;
    if needs_nl {
        f.write_all(b"\n").await?;
    }
    let extra_bytes = tokio::fs::read(extra).await?;
    f.write_all(&extra_bytes).await?;
    f.flush().await?;
    Ok(())
}

/// The Photon import argument vector. Pure, so the command shape is unit-tested
/// even though Photon runs in the build image.
fn photon_import_args(
    heap: &str,
    jar: &str,
    import_file: &Path,
    data_dir: &Path,
    country_codes: &str,
    languages: &str,
) -> Vec<String> {
    vec![
        format!("-Xmx{heap}"),
        "-jar".to_string(),
        jar.to_string(),
        "import".to_string(),
        "-import-file".to_string(),
        import_file.display().to_string(),
        "-data-dir".to_string(),
        data_dir.display().to_string(),
        "-country-codes".to_string(),
        country_codes.to_string(),
        "-languages".to_string(),
        languages.to_string(),
        // Keep the id/image back-links so the enrichment layer can resolve them.
        "-extra-tags".to_string(),
        "wikidata,wikipedia,wikimedia_commons".to_string(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn import_args_carry_subcommand_file_filters_and_extra_tags() {
        let args = photon_import_args(
            "2g",
            "/opt/photon.jar",
            Path::new("/data/photon/import.jsonl"),
            Path::new("/data/photon"),
            "it",
            "it,en",
        );
        assert_eq!(args[0], "-Xmx2g");
        // git-style subcommand, not the pre-1.0 `-nominatim-import` flag.
        assert!(args.iter().any(|a| a == "import"));
        let i = args.iter().position(|a| a == "-import-file").unwrap();
        assert_eq!(args[i + 1], "/data/photon/import.jsonl");
        let d = args.iter().position(|a| a == "-data-dir").unwrap();
        assert_eq!(args[d + 1], "/data/photon");
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-country-codes" && w[1] == "it")
        );
        assert!(
            args.windows(2)
                .any(|w| w[0] == "-languages" && w[1] == "it,en")
        );
        let e = args.iter().position(|a| a == "-extra-tags").unwrap();
        assert_eq!(args[e + 1], "wikidata,wikipedia,wikimedia_commons");
    }

    #[tokio::test]
    async fn append_inserts_missing_newline_between_files() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("import.jsonl");
        let civici = dir.path().join("civici.jsonl");
        // base has NO trailing newline — the corruption case the fix guards.
        tokio::fs::write(&base, b"{\"dump\":1}").await.unwrap();
        tokio::fs::write(&civici, b"{\"civico\":1}\n")
            .await
            .unwrap();

        append_with_newline(&base, &civici).await.unwrap();

        let merged = tokio::fs::read_to_string(&base).await.unwrap();
        assert_eq!(merged, "{\"dump\":1}\n{\"civico\":1}\n");
        // exactly two lines — the docs did not glue onto one.
        assert_eq!(merged.lines().count(), 2);
    }

    #[tokio::test]
    async fn append_keeps_single_newline_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("import.jsonl");
        let civici = dir.path().join("civici.jsonl");
        tokio::fs::write(&base, b"{\"dump\":1}\n").await.unwrap();
        tokio::fs::write(&civici, b"{\"civico\":1}\n")
            .await
            .unwrap();

        append_with_newline(&base, &civici).await.unwrap();

        let merged = tokio::fs::read_to_string(&base).await.unwrap();
        assert_eq!(merged, "{\"dump\":1}\n{\"civico\":1}\n");
    }
}
