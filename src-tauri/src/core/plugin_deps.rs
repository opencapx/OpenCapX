//! F5 — Pure function layer for inter-plugin dependencies: parsing / missing detection / cycle detection.
//! Why a separate module: these decisions are reused on both the install and startup paths, and must be unit-testable without a store.

use std::collections::{BTreeMap, BTreeSet};

/// manifest dependencies → parsed requirement table. Bad entries are skipped (strict rejection is validate_manifest's job).
pub fn parse_deps(raw: &BTreeMap<String, String>) -> BTreeMap<String, semver::VersionReq> {
    raw.iter()
        .filter_map(|(k, v)| semver::VersionReq::parse(v).ok().map(|r| (k.clone(), r)))
        .collect()
}

/// Unmet dependencies → `(depId, raw requirement text from manifest)`. An unparseable requirement (theoretically caught by validate,
/// this is defensive) is treated as unmet — fail-closed, safe direction: report more missing rather than let it through.
pub fn find_missing(
    raw: &BTreeMap<String, String>,
    installed: &[(String, String)],
) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (id, req_str) in raw {
        let satisfied = semver::VersionReq::parse(req_str)
            .ok()
            .is_some_and(|req| {
                installed.iter().find(|(iid, _)| iid == id).is_some_and(|(_, ver)| {
                    super::marketplace::parse_version_lenient(ver).is_some_and(|v| req.matches(&v))
                })
            });
        if !satisfied {
            out.push((id.clone(), req_str.clone()));
        }
    }
    out
}

/// Splice the new plugin (dependency graph) into the installed graph and detect a cycle. Returns the cycle path (with the same id at both ends).
pub fn would_create_cycle(
    new_id: &str,
    new_deps: &BTreeMap<String, semver::VersionReq>,
    installed_deps: &[(String, BTreeMap<String, semver::VersionReq>)],
) -> Option<Vec<String>> {
    // DFS: start from new_id, follow dependencies (only through the installed graph; returning to new_id means a cycle).
    // `visited` pruning: if the installed graph already contains a cycle (b→c→b), without this guard the recursion never terminates.
    fn walk(
        cur: &str,
        target: &str,
        installed: &[(String, BTreeMap<String, semver::VersionReq>)],
        path: &mut Vec<String>,
        visited: &mut BTreeSet<String>,
    ) -> Option<Vec<String>> {
        if !visited.insert(cur.to_string()) {
            return None; // Node already explored: cycle or repeated path, prune it
        }
        let deps = installed.iter().find(|(id, _)| id == cur).map(|(_, d)| d)?;
        for next in deps.keys() {
            path.push(next.clone());
            if next == target {
                return Some(path.clone());
            }
            if let Some(found) = walk(next, target, installed, path, visited) {
                return Some(found);
            }
            path.pop();
        }
        None
    }
    let mut path = vec![new_id.to_string()];
    let mut visited: BTreeSet<String> = BTreeSet::new();
    visited.insert(new_id.to_string());
    for dep in new_deps.keys() {
        path.push(dep.clone());
        if dep == new_id {
            return Some(path.clone()); // Theoretically caught by validate, defensive fallback
        }
        if let Some(found) = walk(dep, new_id, installed_deps, &mut path, &mut visited) {
            return Some(found);
        }
        path.pop();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn deps(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn find_missing_empty_when_satisfied() {
        let d = deps(&[("com.x.b", ">=1.0.0")]);
        let installed = vec![("com.x.b".to_string(), "1.2.0".to_string())];
        assert!(find_missing(&d, &installed).is_empty());
    }

    #[test]
    fn find_missing_reports_absent_and_low_version() {
        let d = deps(&[("com.x.b", ">=1.2.0"), ("com.x.c", "^0.9")]);
        let installed = vec![
            ("com.x.b".to_string(), "1.1.0".to_string()), // below the lower bound
            // com.x.c not installed
        ];
        let mut missing = find_missing(&d, &installed);
        missing.sort();
        assert_eq!(missing, vec![
            ("com.x.b".to_string(), ">=1.2.0".to_string()),
            ("com.x.c".to_string(), "^0.9".to_string()),
        ]);
    }

    /// The reported text must be the original manifest string, not the semver-normalized form ("1.2" must not become "^1.2").
    #[test]
    fn find_missing_preserves_raw_requirement_text() {
        let d = deps(&[("com.x.b", "1.2")]);
        let installed: Vec<(String, String)> = vec![];
        assert_eq!(
            find_missing(&d, &installed),
            vec![("com.x.b".to_string(), "1.2".to_string())]
        );
    }

    /// Unparseable requirements are fail-closed: even a seemingly satisfying installed version is reported as missing.
    #[test]
    fn find_missing_treats_unparseable_as_missing() {
        let d = deps(&[("com.x.b", "not a req")]);
        let installed = vec![("com.x.b".to_string(), "9.9.9".to_string())];
        assert_eq!(
            find_missing(&d, &installed),
            vec![("com.x.b".to_string(), "not a req".to_string())]
        );
    }

    #[test]
    fn cycle_detects_back_dependency_on_new_plugin() {
        // Installing new plugin a (depends on b); installed b's manifest depends on a → cycle a→b→a
        let new_deps = parse_deps(&deps(&[("com.x.b", ">=1.0.0")]));
        let installed = vec![(
            "com.x.b".to_string(),
            parse_deps(&deps(&[("com.x.a", ">=0.1.0")])),
        )];
        let cyc = would_create_cycle("com.x.a", &new_deps, &installed).expect("cycle");
        assert_eq!(cyc.first().map(String::as_str), Some("com.x.a"));
        assert!(cyc.len() >= 3, "at least the three hops a→b→a: {cyc:?}");
    }

    #[test]
    fn cycle_none_for_forest_and_for_deep_chain() {
        let new_deps = parse_deps(&deps(&[("com.x.b", ">=1.0.0")]));
        let installed = vec![
            ("com.x.b".to_string(), parse_deps(&deps(&[("com.x.c", "^1.0")]))),
            ("com.x.c".to_string(), BTreeMap::new()),
        ];
        assert!(would_create_cycle("com.x.a", &new_deps, &installed).is_none());
    }

    /// Hardening: when the installed graph already contains a cycle (legacy data), DFS must prune and terminate, not recurse forever.
    #[test]
    fn cycle_terminates_on_preexisting_installed_cycle() {
        let new_deps = parse_deps(&deps(&[("com.x.b", ">=1.0.0")]));
        let installed = vec![
            ("com.x.b".to_string(), parse_deps(&deps(&[("com.x.c", "^1.0")]))),
            ("com.x.c".to_string(), parse_deps(&deps(&[("com.x.b", "^1.0")]))),
        ];
        assert!(would_create_cycle("com.x.a", &new_deps, &installed).is_none());
    }
}
