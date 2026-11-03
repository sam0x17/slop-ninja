//! Apply the original coordinate catalog to additional author panels without fitting it.
use anyhow::{Result, ensure};
use grammar_core::space::Space;
use sha2::{Digest, Sha256};

#[allow(dead_code)]
#[path = "coordinate_frontier.rs"]
mod frontier;
pub use frontier::Catalog;

pub const CATALOG_SHA256: &str = "170b7f3572c910110752930c8e2d067cbb420f720ebba8fa3ac455cd6fe03b79";
pub const SUBSET_NAME: &str = "all_m2";
pub const SUBSET_INDICES_SHA256: &str =
    "483c24580787371ac28f14cc52a7beafa28b264fa270d99645c9c4cde0d34f19";
pub const TRAINING_CATALOG_POLICY: &str = "apply_frozen_catalog_without_fitting";

pub fn load_catalog(space: &Space, bytes: &[u8]) -> Result<Catalog> {
    ensure!(
        hex::encode(Sha256::digest(bytes)) == CATALOG_SHA256,
        "fixed original catalog bytes changed"
    );
    let catalog: Catalog = serde_json::from_slice(bytes)?;
    catalog.validate(space)?;
    let subset = catalog
        .subsets
        .get(SUBSET_NAME)
        .ok_or_else(|| anyhow::anyhow!("missing fixed subset"))?;
    ensure!(
        catalog.coordinates.len() == 7353
            && catalog.training_authors.len() == 100
            && catalog.training_posts == 1224
            && subset.coordinate_count == 3557
            && subset.canonical_name == SUBSET_NAME
            && subset.indices_sha256 == SUBSET_INDICES_SHA256,
        "fixed catalog/subset identity differs"
    );
    Ok(catalog)
}

pub fn validate_panel_authors(
    authors: &[String],
    catalog_authors: &[String],
    training: bool,
) -> Result<()> {
    ensure!(
        authors.len() >= 2 && authors.windows(2).all(|pair| pair[0] < pair[1]),
        "panel author IDs duplicated or unsorted"
    );
    ensure!(
        catalog_authors.windows(2).all(|pair| pair[0] < pair[1]),
        "catalog authors unsorted"
    );
    ensure!(
        authors
            .iter()
            .all(|author| catalog_authors.binary_search(author).is_err()),
        "added panel authors overlap original catalog fitting authors"
    );
    ensure!(
        !training || authors.len() == 100,
        "training panel must retain exactly 100 eligible authors"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn panel_authors_are_distinct_from_catalog_fitting_authors_and_exactly_one_hundred() {
        let authors: Vec<_> = (0..100).map(|i| format!("panel-{i:03}")).collect();
        let fitted = vec!["original-0".into(), "original-1".into()];
        validate_panel_authors(&authors, &fitted, true).unwrap();
        assert!(validate_panel_authors(&authors[..99], &fitted, true).is_err());
        validate_panel_authors(&authors[..99], &fitted, false).unwrap();
        assert!(validate_panel_authors(&authors, &[authors[0].clone()], true).is_err());
        let mut duplicate = authors.clone();
        duplicate[1] = duplicate[0].clone();
        assert!(validate_panel_authors(&duplicate, &fitted, true).is_err());
    }
}
