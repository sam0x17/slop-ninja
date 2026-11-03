//! Pilot file selection: a fixed-seed rank over each crawl's WET paths.
//!
//! `rank(seed, "cc_human_wet_v1", path)` (the domain-separated 64-bit hash of
//! `grammar_a_pools::splits::rank`) orders every WET path of a crawl; the
//! pilot takes the `per_crawl` lowest-ranked eligible paths. A path is
//! eligible when its segment timestamp (the first 14 digits after
//! `CC-MAIN-` in the file name) is before 2022-11-30, which only removes
//! files of CC-MAIN-2022-49; every record is still date-gated on
//! `WARC-Date`. Production replaces the public seed with a secret key.
use anyhow::{Context, Result, ensure};
use grammar_a_pools::splits::rank;
use std::{io::Read, path::Path};

pub const PILOT_SEED: &str = "slop_ninja_cc_human_pilot_v1:2026-10-30";
pub const WET_SCOPE: &str = "cc_human_wet_v1";
pub const SELECTION_SCHEMA: &str = "slop_ninja_cc_human_selection_v1";

/// Segment start timestamp (`YYYYMMDDhhmmss`) from a WET or WARC path.
pub fn segment_stamp(path: &str) -> Option<&str> {
    let name = path.rsplit('/').next()?;
    let rest = name.strip_prefix("CC-MAIN-")?;
    let ts = rest.get(..14)?;
    ts.bytes().all(|b| b.is_ascii_digit()).then_some(ts)
}

pub fn eligible(path: &str) -> bool {
    segment_stamp(path).is_some_and(|t| t < "20221130000000")
}

/// Frame crawls: the first column of `per-crawl.tsv`.
pub fn frame(per_crawl_tsv: &Path) -> Result<Vec<String>> {
    let t = std::fs::read_to_string(per_crawl_tsv).with_context(|| format!("read {}", per_crawl_tsv.display()))?;
    let v: Vec<String> = t
        .lines()
        .filter_map(|l| l.split('\t').next())
        .filter(|c| c.starts_with("CC-MAIN-"))
        .map(str::to_owned)
        .collect();
    ensure!(!v.is_empty(), "no crawls in {}", per_crawl_tsv.display());
    Ok(v)
}

pub fn read_paths(gz: &Path) -> Result<Vec<String>> {
    let mut s = String::new();
    flate2::read::MultiGzDecoder::new(std::fs::File::open(gz).with_context(|| format!("open {}", gz.display()))?).read_to_string(&mut s)?;
    Ok(s.lines().map(str::trim).filter(|l| !l.is_empty()).map(str::to_owned).collect())
}

pub struct Pick {
    pub rank: u64,
    pub path: String,
}

/// The `k` lowest-ranked eligible paths, lowest first, and the eligible count.
pub fn pick(paths: &[String], seed: &str, k: usize) -> (Vec<Pick>, usize) {
    let mut v: Vec<Pick> = paths
        .iter()
        .filter(|p| eligible(p))
        .map(|p| Pick {
            rank: rank(seed, WET_SCOPE, p),
            path: p.clone(),
        })
        .collect();
    let n = v.len();
    v.sort_by(|a, b| a.rank.cmp(&b.rank).then_with(|| a.path.cmp(&b.path)));
    v.truncate(k);
    (v, n)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn stamps_and_eligibility() {
        let a = "crawl-data/CC-MAIN-2022-49/segments/1669446706285.92/wet/CC-MAIN-20221126080725-20221126110725-00000.warc.wet.gz";
        let b = "crawl-data/CC-MAIN-2022-49/segments/1669446711001.28/wet/CC-MAIN-20221205000525-20221205030525-00012.warc.wet.gz";
        let c = "crawl-data/CC-MAIN-2016-07/segments/1454701166261.11/wet/CC-MAIN-20160205193926-00093-ip-10-236-182-209.ec2.internal.warc.wet.gz";
        assert_eq!(segment_stamp(a), Some("20221126080725"));
        assert!(eligible(a) && !eligible(b) && eligible(c));
        let (p, n) = pick(&[a.to_owned(), b.to_owned(), c.to_owned()], PILOT_SEED, 1);
        assert_eq!((p.len(), n), (1, 2));
    }
}
