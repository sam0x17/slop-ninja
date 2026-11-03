//! Single use: a scorer instance serves one artifact for one cohort, admitted
//! by that cohort's artifact spec.
//!
//! The first accepted load binds the instance to its package digest, cohort
//! (an absent cohort is a value too) and artifact spec for the life of the
//! process, whatever the load's outcome. These decisions are the whole policy;
//! `a_scorer.rs` applies them to its routes.
use crate::engine::{UNNAMED_LOAD_SPEC, admits_by};
use slop_ninja_artifact_check::spec::Version;
use std::collections::BTreeSet;

/// What an instance is bound to.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Binding {
    pub package: String,
    pub cohort: Option<String>,
    /// The artifact spec the load admits by: the one the load names
    /// (`spec=<digest>`, the cohort's), else [`UNNAMED_LOAD_SPEC`].
    pub spec: Version,
}

impl Binding {
    /// Whether a score request for `package` and `cohort` is this binding's.
    /// Score requests name no spec: the load fixed it.
    pub fn serves(&self, package: &str, cohort: Option<&str>) -> bool {
        self.package == package && self.cohort.as_deref() == cohort
    }
}

/// A cohort id: the coordinator's cohort label rule
/// (`subnet/coordinator/src/eval/plan.rs`, plan validation: 1 to 96 bytes of
/// `[A-Za-z0-9-]`), so any label the coordinator issues binds as itself.
pub fn valid_cohort(c: &str) -> bool {
    !c.is_empty() && c.len() <= 96 && c.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

/// The spec version whose digest (`slop_ninja_artifact_check spec`, the
/// digest a cohort's `artifact_spec` names) is `sha256`, when this build
/// defines one.
pub fn spec_by_digest(sha256: &str) -> Option<Version> {
    Version::ALL.into_iter().find(|v| v.spec_sha256() == sha256)
}

/// Why a load query is refused: `query` for its form, `spec` for a spec digest
/// this build does not define or does not admit by ([`admits_by`]). Neither
/// is the artifact's fault.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum QueryError {
    Query,
    Spec,
}

impl QueryError {
    pub fn code(self) -> &'static str {
        match self {
            QueryError::Query => "query",
            QueryError::Spec => "spec",
        }
    }
}

/// `package`, an optional `cohort` and an optional `spec` (a spec digest)
/// from a load query, each at most once; any other key, a repeated key or a
/// malformed cohort refuses the query. A load that names no spec is admitted
/// by [`UNNAMED_LOAD_SPEC`].
pub fn load_query(query: &str) -> Result<Binding, QueryError> {
    let (mut package, mut cohort, mut spec) = (None, None, None);
    for kv in query.split('&').filter(|kv| !kv.is_empty()) {
        match kv.split_once('=').ok_or(QueryError::Query)? {
            ("package", v) if package.is_none() => package = Some(v.to_string()),
            ("cohort", v) if cohort.is_none() && valid_cohort(v) => cohort = Some(v.to_string()),
            ("spec", v) if spec.is_none() => {
                spec = Some(spec_by_digest(v).filter(|s| admits_by(*s)).ok_or(QueryError::Spec)?)
            }
            _ => return Err(QueryError::Query),
        }
    }
    Ok(Binding { package: package.ok_or(QueryError::Query)?, cohort, spec: spec.unwrap_or(UNNAMED_LOAD_SPEC) })
}

/// What a load request (or a score request's package and cohort) meets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Load {
    /// Unbound: this request may bind it.
    Fresh,
    /// Bound to exactly this package and cohort.
    Same,
    /// Bound to something else: refused with `instance_bound`.
    Bound,
}

impl Load {
    pub fn decide(current: Option<&Binding>, want: &Binding) -> Self {
        match current {
            None => Load::Fresh,
            Some(b) if b == want => Load::Same,
            Some(_) => Load::Bound,
        }
    }
}

/// What a part upload meets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Put {
    /// Unbound: store and check the part.
    Store,
    /// Bound, and the part belongs to the bound package: nothing to store.
    AlreadyBound,
    /// Bound, and the part is not the bound package's: refused.
    Refuse,
}

impl Put {
    pub fn decide(current: Option<&Binding>, bound_parts: &BTreeSet<String>, sha: &str) -> Self {
        match current {
            None => Put::Store,
            Some(_) if bound_parts.contains(sha) => Put::AlreadyBound,
            Some(_) => Put::Refuse,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn b(p: &str, c: Option<&str>) -> Binding {
        Binding { package: p.into(), cohort: c.map(str::to_owned), spec: UNNAMED_LOAD_SPEC }
    }

    fn bs(p: &str, c: Option<&str>, spec: Version) -> Binding {
        Binding { spec, ..b(p, c) }
    }

    #[test]
    fn a_bound_instance_serves_only_its_package_and_cohort() {
        let x = b("aa", None);
        assert_eq!(Load::decide(None, &x), Load::Fresh);
        assert_eq!(Load::decide(Some(&x), &x), Load::Same);
        // Another artifact, the same artifact for a cohort, or for another cohort.
        assert_eq!(Load::decide(Some(&x), &b("bb", None)), Load::Bound);
        assert_eq!(Load::decide(Some(&x), &b("aa", Some("c1"))), Load::Bound);
        let y = b("aa", Some("c1"));
        assert_eq!(Load::decide(Some(&y), &y), Load::Same);
        assert_eq!(Load::decide(Some(&y), &b("aa", Some("c2"))), Load::Bound);
        assert_eq!(Load::decide(Some(&y), &b("aa", None)), Load::Bound);
        // The same package and cohort under another spec.
        assert_eq!(Load::decide(Some(&y), &bs("aa", Some("c1"), Version::V3)), Load::Bound);
        // A score request names the package and cohort; the load fixed the spec.
        let z = bs("aa", Some("c1"), Version::V3);
        assert!(z.serves("aa", Some("c1")) && !z.serves("aa", None));
        assert!(!z.serves("aa", Some("c2")) && !z.serves("bb", Some("c1")));
    }

    #[test]
    fn parts_after_binding_are_refused_unless_they_are_the_bound_packages() {
        let x = b("aa", None);
        let parts: BTreeSet<String> = ["p1".to_string()].into();
        assert_eq!(Put::decide(None, &parts, "p9"), Put::Store);
        assert_eq!(Put::decide(Some(&x), &parts, "p1"), Put::AlreadyBound);
        assert_eq!(Put::decide(Some(&x), &parts, "p9"), Put::Refuse);
    }

    #[test]
    fn load_queries_name_a_package_and_at_most_one_wellformed_cohort_and_spec() {
        assert_eq!(load_query("package=ab"), Ok(b("ab", None)));
        assert_eq!(load_query("package=ab&cohort=testnet-rv1-a"), Ok(b("ab", Some("testnet-rv1-a"))));
        assert_eq!(load_query("cohort=c-1&package=ab"), Ok(b("ab", Some("c-1"))));
        for bad in ["", "cohort=c", "package=a&package=b", "package=a&cohort=c&cohort=d", "package=a&x=1",
            "package=a&cohort=", "package=a&cohort=a/b", "package=a&cohort=c.1", "package=a&cohort=c:1",
            "package=a&cohort=c_1", "package"] {
            assert_eq!(load_query(bad), Err(QueryError::Query), "{bad}");
        }
        assert!(valid_cohort(&"c".repeat(96)) && !valid_cohort(&"c".repeat(97)));
        // A spec is named by its digest; a load that names none is admitted by spec v2.
        assert_eq!(UNNAMED_LOAD_SPEC, Version::V2);
        for v in [Version::V2, Version::V3] {
            let d = v.spec_sha256();
            assert_eq!(load_query(&format!("package=ab&cohort=c1&spec={d}")), Ok(bs("ab", Some("c1"), v)));
            assert_eq!(load_query(&format!("spec={d}&package=ab")), Ok(bs("ab", None, v)));
            assert_eq!(load_query(&format!("package=ab&spec={d}&spec={d}")), Err(QueryError::Query));
        }
        // Spec v1 bounds no JSON file, so the scorer does not admit by it; an
        // unknown digest, an id or a short form names no spec this build admits by.
        let v1 = Version::V1.spec_sha256();
        for bad in [v1.as_str(), &"0".repeat(64), "slop_ninja_a_artifact_spec_v2", "v3", ""] {
            assert_eq!(load_query(&format!("package=ab&spec={bad}")), Err(QueryError::Spec), "{bad}");
        }
        assert_eq!(spec_by_digest(&Version::V3.spec_sha256()), Some(Version::V3));
    }
}
