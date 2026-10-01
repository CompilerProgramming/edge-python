use alloc::string::String;

use crate::s;
use super::manifest::Manifest;

// The engine this build is, the version every floor is held to.
pub const ENGINE: &str = env!("CARGO_PKG_VERSION");

// A description reads as one line in a listing, past this it is a readme.
pub const MAX_DESCRIPTION: usize = 60;
// A name reads the same in a url, an import and a listing.
pub const MAX_NAME: usize = 40;
pub const MAX_REPOSITORY: usize = 256;

// The holders a permissions section names beside packages, so no package may be named any of them.
pub const HOLDERS: [&str; 3] = ["all", "main", "eval"];

const VERSION_RULE: &str = "major.minor.patch, each part 0 to 99 without leading zeros";

/* Each part 0 to 99 written plainly, so a release has exactly one spelling and orders the same everywhere. */
pub fn is_version(v: &str) -> bool {
    let mut parts = v.split('.');
    let plain = |p: &str| p == "0" || (matches!(p.len(), 1 | 2) && !p.starts_with('0') && p.bytes().all(|b| b.is_ascii_digit()));
    let three = (parts.next(), parts.next(), parts.next(), parts.next());
    matches!(three, (Some(a), Some(b), Some(c), None) if plain(a) && plain(b) && plain(c))
}

/* Three runs of digits between dots, what a target reads as before its parts meet the rule. */
pub fn shaped_like_version(v: &str) -> bool {
    let parts: alloc::vec::Vec<&str> = v.split('.').collect();
    parts.len() == 3 && parts.iter().all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

/* The three numbers of a version, each its own integer, so 0.7.0 outranks 0.6.45 rather than reading as a decimal. */
pub fn parts(v: &str) -> (u32, u32, u32) {
    let mut read = v.split('.').map(|part| part.parse().unwrap_or(0));
    (read.next().unwrap_or(0), read.next().unwrap_or(0), read.next().unwrap_or(0))
}

/// Whether `floor` asks for an engine later than `engine`.
pub fn newer(floor: &str, engine: &str) -> bool {
    parts(floor) > parts(engine)
}

/* Why this engine cannot run a manifest written for a later one, None when it can. */
pub fn floor_error(m: &Manifest) -> Option<String> {
    let floor = m.edge.as_deref()?;
    newer(floor, ENGINE).then(|| s!("needs edge ", str floor, ", this is ", str ENGINE))
}

/* A name that reads the same in a url, an import and a listing. */
pub fn named(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name.starts_with(|c: char| c.is_ascii_lowercase())
        && !name.ends_with('-')
        && !name.contains("--")
        && name.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/* An address a listing can open, so an ssh remote or a bare host is refused. */
pub fn linked(url: &str) -> bool {
    let Some(host) = url.strip_prefix("https://") else { return false };
    !host.is_empty() && url.len() <= MAX_REPOSITORY && !url.contains(char::is_whitespace)
}

/* Why no package may take `name`, None when one may. */
pub fn name_error(name: &str, system: &[&str]) -> Option<String> {
    if !named(name) {
        return Some(s!("name '", str name, "' must be lowercase letters, digits and single hyphens, starting with a letter, ", int MAX_NAME, " at most"));
    }
    if HOLDERS.contains(&name) {
        return Some(s!("name '", str name, "' is reserved for permissions"));
    }
    system.contains(&name).then(|| s!("name '", str name, "' is reserved for the system module of that name"))
}

/* Why a declared import cannot stand, a system module's name taken or a version spelled against the rule. */
pub fn import_error(name: &str, target: &str, system: &[&str]) -> Option<String> {
    if system.contains(&name) {
        return Some(s!("import '", str name, "' takes the name of a system module, grant it under permissions instead"));
    }
    (shaped_like_version(target) && !is_version(target)).then(|| s!("'", str name, "' is declared '", str target, "', and a version is ", str VERSION_RULE))
}

/* Every field a registry reads and every import, held to the rules a package is packed under. */
pub fn check(m: &Manifest, system: &[&str]) -> Result<(), String> {
    if let Some(name) = &m.name
        && let Some(e) = name_error(name, system)
    {
        return Err(e);
    }
    if let Some(version) = &m.version
        && !is_version(version)
    {
        return Err(s!("version '", str version, "' must be ", str VERSION_RULE));
    }
    if let Some(description) = &m.description {
        let len = description.chars().count();
        if description.trim().is_empty() {
            return Err(s!("description is empty"));
        }
        if description.contains('\n') {
            return Err(s!("description must be one line"));
        }
        if len > MAX_DESCRIPTION {
            return Err(s!("description is ", int len, " characters, the cap is ", int MAX_DESCRIPTION));
        }
    }
    if let Some(repository) = &m.repository
        && !linked(repository)
    {
        return Err(s!("repository '", str repository, "' must be an https url a listing can link"));
    }
    if let Some(edge) = &m.edge
        && !is_version(edge)
    {
        return Err(s!("edge '", str edge, "' must be ", str VERSION_RULE));
    }
    m.imports.iter().find_map(|(name, target)| import_error(name, target, system)).map_or(Ok(()), Err)
}

/* A manifest a package carries, held to the rules and with every version it declares resolved by the lock beside it. */
pub fn check_package(manifest: &[u8], lock: Option<&[u8]>, system: &[&str]) -> Result<(), String> {
    let m = super::parse_manifest(manifest)?;
    check(&m, system)?;
    let held = lock.map(super::lock::Lock::parse).transpose()?;
    if let Some(held) = &held {
        held.check()?;
    }
    m.imports.iter().try_for_each(|(name, target)| super::lock::locked_spec(name, target, held.as_ref()).map(|_| ()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modules::parse_manifest;

    const SYSTEM: [&str; 2] = ["net", "time"];

    #[test]
    fn a_version_has_one_spelling_with_parts_up_to_99() {
        for v in ["0.1.0", "10.99.0", "0.0.0", "99.99.99"] {
            assert!(is_version(v), "{v}");
        }
        for v in ["01.2.3", "1.2.100", "1.2.3-beta", "v1.2.3", "1.2", "1.2.3.4", "", "1..3", "1.2.-3"] {
            assert!(!is_version(v), "{v}");
        }
    }

    // Each field is its own integer, so a patch of 45 sits below a minor of 7 rather than reading as a decimal.
    #[test]
    fn a_version_orders_by_its_numbers() {
        assert!(newer("0.7.0", "0.6.45") && !newer("0.6.5", "0.6.41") && newer("1.0.0", "0.99.99"));
        assert!(!newer(ENGINE, ENGINE));
    }

    #[test]
    fn a_floor_above_this_engine_says_both_versions() {
        let later = parse_manifest(br#"{ "edge": "99.0.0" }"#).unwrap();
        assert_eq!(floor_error(&later), Some(s!("needs edge 99.0.0, this is ", str ENGINE)));
        assert_eq!(floor_error(&parse_manifest(br#"{ "edge": "0.0.1" }"#).unwrap()), None);
        assert_eq!(floor_error(&parse_manifest(b"{}").unwrap()), None);
    }

    #[test]
    fn the_fields_a_registry_would_turn_away_are_refused() {
        for (body, want) in [
            (r#"{ "name": "Slugify" }"#, "must be lowercase"),
            (r#"{ "name": "slug--ify" }"#, "must be lowercase"),
            (r#"{ "name": "slugify-" }"#, "must be lowercase"),
            (r#"{ "name": "a-name-far-too-long-for-any-listing-to-show" }"#, "must be lowercase"),
            (r#"{ "name": "main" }"#, "is reserved for permissions"),
            (r#"{ "name": "eval" }"#, "is reserved for permissions"),
            (r#"{ "name": "time" }"#, "is reserved for the system module"),
            (r#"{ "version": "1.0" }"#, "must be major.minor.patch"),
            (r#"{ "version": "01.2.3" }"#, "without leading zeros"),
            (r#"{ "edge": "0.7.100" }"#, "edge '0.7.100' must be"),
            (r#"{ "description": "  " }"#, "description is empty"),
            (r#"{ "description": "two\nlines" }"#, "must be one line"),
            (r#"{ "description": "Turn absolutely any text that you have into a tidy url slug fast." }"#, "the cap is 60"),
            (r#"{ "repository": "git@github.com:x/slugify.git" }"#, "must be an https url"),
            (r#"{ "imports": { "time": "./fake.py" } }"#, "takes the name of a system module"),
            (r#"{ "imports": { "json": "0.01.0" } }"#, "is declared '0.01.0'"),
        ] {
            let err = check(&parse_manifest(body.as_bytes()).unwrap(), &SYSTEM).unwrap_err();
            assert!(err.contains(want), "{body} wanted '{want}', got '{err}'");
        }
        let fine = r#"{ "name": "slugify", "version": "0.1.0", "description": "Turn text into a slug.", "repository": "https://github.com/x/slugify", "imports": { "json": "0.1.0", "lib": "./lib.py" } }"#;
        assert!(check(&parse_manifest(fine.as_bytes()).unwrap(), &SYSTEM).is_ok());
    }

    // A version says nothing about where its bytes are, so its package carries the lock.
    #[test]
    fn a_package_declaring_a_version_carries_its_lock() {
        let manifest = br#"{ "name": "app", "imports": { "dep": "0.1.0" } }"#;
        let held = br#"{ "dep": { "version": "0.1.0", "url": "https://example.com/dep.edge", "digest": "sha256-a4bf0317b809477e6e475cd791573953d3ae05c4e2b03bd5f35199609557dd90" } }"#;
        assert!(check_package(manifest, Some(held), &SYSTEM).is_ok());
        assert_eq!(check_package(manifest, None, &SYSTEM).unwrap_err(), "'dep' is not locked, run edge lock");
        assert!(check_package(br#"{ "imports": { "dep": "0.2.0" } }"#, Some(held), &SYSTEM).unwrap_err().contains("locked 0.1.0"));
        assert!(check_package(manifest, Some(b"{ nope"), &SYSTEM).is_err());
        let short = br#"{ "dep": { "version": "0.1.0", "url": "https://example.com/dep.edge", "digest": "sha256-ab" } }"#;
        assert!(check_package(manifest, Some(short), &SYSTEM).unwrap_err().contains("64 hex characters"));
    }
}
