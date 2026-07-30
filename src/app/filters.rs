use crate::{
    domain::{HostFilter, HostStatus, UsageCollectionStatus, UsageFilter},
    modules,
};
use chrono::Utc;
use std::{
    collections::{BTreeMap, HashMap},
    fmt,
    sync::OnceLock,
    time::Duration,
};
use thiserror::Error;

// ---------------------------------------------------------------------------
// Public catalog types (still part of the API surface that /v1/namespaces and
// the OpenAPI schema serialise from).
// ---------------------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct FilterNamespaceCatalog {
    pub bare: BTreeMap<&'static str, FilterKeyMetadata>,
    pub namespaces: BTreeMap<&'static str, BTreeMap<&'static str, FilterKeyMetadata>>,
    pub examples: Vec<&'static str>,
}

#[derive(Clone, Debug)]
pub struct FilterKeyMetadata {
    pub value_type: &'static str,
    pub values: Vec<&'static str>,
    pub description: &'static str,
}

// ---------------------------------------------------------------------------
// Filter key registry — the single source of truth.
//
// Every supported filter is declared exactly once below. The parser, the
// namespace catalog, and the textual help in error messages are all derived
// from this table. To add a filter, add a row.
//
// The only piece of the system that still hardcodes filter strings is the
// per-route `#[utoipa::path(params(...))]` block in `src/api/routes/mod.rs`,
// which utoipa needs as literal tokens at macro time.
// ---------------------------------------------------------------------------

/// Builders that mirror the final filter shapes. Apply functions write into
/// these, then `into_host` / `into_usage` finalises them.
#[derive(Default)]
struct HostFilterBuilder {
    icmp_status: Option<HostStatus>,
    group: Option<String>,
    metadata: HashMap<String, String>,
    usage: UsageFilterBuilder,
}

#[derive(Default)]
struct UsageFilterBuilder {
    status: Option<UsageCollectionStatus>,
    inactive_console_for: Option<Duration>,
    no_users_for: Option<Duration>,
}

impl HostFilterBuilder {
    fn into_host(self) -> HostFilter {
        HostFilter {
            icmp_status: self.icmp_status,
            group: self.group,
            metadata: self.metadata,
            usage: self.usage.into_usage(),
        }
    }
}

impl UsageFilterBuilder {
    fn into_usage(self) -> UsageFilter {
        UsageFilter {
            status: self.status,
            inactive_console_for: self.inactive_console_for,
            no_users_for: self.no_users_for,
            now: Utc::now(),
        }
    }
}

/// What to do with a (namespace, key, value) match.
enum ApplyKind {
    /// Write into a host-level field via the builder.
    Host(fn(&mut HostFilterBuilder, &str) -> Result<(), FilterParseError>),
    /// Write into the nested usage filter. Same callback works whether the
    /// outer parser is host-level or the standalone usage parser — the
    /// dispatch table sees both shapes.
    Usage(fn(&mut UsageFilterBuilder, &str) -> Result<(), FilterParseError>),
    /// Special-case: `metadata.<anything>` — the key part of the URL becomes
    /// the metadata field name.
    MetadataWildcard,
}

struct FilterEntry {
    /// `None` means bare (no namespace). `Some("usage")` etc. names the namespace.
    namespace: Option<&'static str>,
    /// `"*"` is reserved for wildcard entries (metadata).
    key: &'static str,
    apply: ApplyKind,
}

// The static table. Keep alphabetised by (namespace, key) for readability.
const REGISTRY: &[FilterEntry] = &[
    FilterEntry {
        namespace: None,
        key: "group",
        apply: ApplyKind::Host(|b, v| {
            b.group = Some(v.to_string());
            Ok(())
        }),
    },
    FilterEntry {
        namespace: Some("icmp"),
        key: "status",
        apply: ApplyKind::Host(|b, v| {
            b.icmp_status = Some(parse_icmp_status("icmp.status", v)?);
            Ok(())
        }),
    },
    FilterEntry {
        namespace: Some("metadata"),
        key: "*",
        apply: ApplyKind::MetadataWildcard,
    },
    FilterEntry {
        namespace: Some("usage"),
        key: "status",
        apply: ApplyKind::Usage(|b, v| {
            b.status = Some(parse_usage_status("usage.status", v)?);
            Ok(())
        }),
    },
    FilterEntry {
        namespace: Some("usage"),
        key: "inactive_console_for",
        apply: ApplyKind::Usage(|b, v| {
            b.inactive_console_for = Some(parse_duration_query("usage.inactive_console_for", v)?);
            Ok(())
        }),
    },
    FilterEntry {
        namespace: Some("usage"),
        key: "no_users_for",
        apply: ApplyKind::Usage(|b, v| {
            b.no_users_for = Some(parse_duration_query("usage.no_users_for", v)?);
            Ok(())
        }),
    },
];

const EXAMPLES: &[&str] = &[
    "/v1/hosts?icmp.status=down&group=core",
    "/v1/hosts?metadata.room=net-a",
    "/v1/hosts?usage.status=error",
    "/v1/hosts?icmp.status=up&group=core&usage.no_users_for=7days",
];

impl Default for FilterNamespaceCatalog {
    fn default() -> Self {
        let mut bare: BTreeMap<&'static str, FilterKeyMetadata> = BTreeMap::new();
        let mut namespaces: BTreeMap<&'static str, BTreeMap<&'static str, FilterKeyMetadata>> =
            BTreeMap::new();
        bare.insert(
            "group",
            FilterKeyMetadata {
                value_type: "string",
                values: vec![],
                description: "host must belong to the named group",
            },
        );
        namespaces.entry("metadata").or_default().insert(
            "*",
            FilterKeyMetadata {
                value_type: "string",
                values: vec![],
                description: "host metadata field must match exactly, for example metadata.room=net-a",
            },
        );
        for module in modules::registry() {
            let metadata = module.metadata();
            for (key, filter) in supported_module_filter_specs(metadata.id, module.filter_specs()) {
                namespaces
                    .entry(metadata.id)
                    .or_default()
                    .insert(key, filter);
            }
        }
        Self {
            bare,
            namespaces,
            examples: EXAMPLES.to_vec(),
        }
    }
}

// ---------------------------------------------------------------------------
// Error type. Help strings are still computed from the catalog (which is now
// derived from REGISTRY), so REGISTRY is the only place that knows the names.
// ---------------------------------------------------------------------------

#[derive(Debug, Error, Eq, PartialEq)]
pub enum FilterParseError {
    #[error("invalid filter key {key:?}; expected either group or namespace.key")]
    InvalidKey { key: String },
    #[error("unknown filter key {key:?}; valid bare keys: {bare}; valid namespaces: {namespaces}")]
    UnknownBareKey {
        key: String,
        bare: String,
        namespaces: String,
    },
    #[error("unknown filter namespace {namespace:?}; valid namespaces: {namespaces}")]
    UnknownNamespace {
        namespace: String,
        namespaces: String,
    },
    #[error("unknown filter key {namespace}.{key}; valid keys for {namespace}: {valid}")]
    UnknownNamespaceKey {
        namespace: String,
        key: String,
        valid: String,
    },
    #[error("{name}={value:?}: {message}")]
    InvalidValue {
        name: String,
        value: String,
        message: String,
    },
}

fn catalog() -> &'static FilterNamespaceCatalog {
    static CATALOG: OnceLock<FilterNamespaceCatalog> = OnceLock::new();
    CATALOG.get_or_init(FilterNamespaceCatalog::default)
}

fn known_bare_keys() -> String {
    join(catalog().bare.keys().copied())
}

fn known_namespaces() -> String {
    join(catalog().namespaces.keys().copied())
}

fn known_namespace_keys(namespace: &str) -> String {
    match catalog().namespaces.get(namespace) {
        Some(map) => {
            if map.contains_key("*") {
                "any field name".into()
            } else {
                join(map.keys().copied())
            }
        }
        None => String::new(),
    }
}

fn join<'a, I: IntoIterator<Item = &'a str>>(items: I) -> String {
    let collected: Vec<&str> = items.into_iter().collect();
    collected.join(", ")
}

pub fn namespace_catalog() -> FilterNamespaceCatalog {
    catalog().clone()
}

pub fn supported_module_filter_specs(
    module_id: &'static str,
    specs: Vec<(&'static str, FilterKeyMetadata)>,
) -> Vec<(&'static str, FilterKeyMetadata)> {
    specs
        .into_iter()
        .filter(|(key, _)| lookup(Some(module_id), key).is_some())
        .collect()
}

// ---------------------------------------------------------------------------
// Registry lookup. Linear scan — the table is tiny (<10 rows) and the cost
// is dwarfed by URL parsing.
// ---------------------------------------------------------------------------

fn lookup(namespace: Option<&str>, key: &str) -> Option<&'static FilterEntry> {
    // Exact match first (handles every non-metadata entry).
    if let Some(entry) = REGISTRY
        .iter()
        .find(|e| e.namespace == namespace && e.key == key)
    {
        return Some(entry);
    }
    // Wildcard fallback (currently only metadata.*).
    REGISTRY
        .iter()
        .find(|e| e.namespace == namespace && e.key == "*")
}

// ---------------------------------------------------------------------------
// Parsers. Both delegate to REGISTRY.
// ---------------------------------------------------------------------------

pub fn parse_host_filter(params: HashMap<String, String>) -> Result<HostFilter, FilterParseError> {
    let mut builder = HostFilterBuilder::default();
    for (raw_key, value) in params {
        let (namespace, key) = match split_filter_key(&raw_key)? {
            FilterKey::Bare(k) => (None, k),
            FilterKey::Namespaced { namespace, key } => (Some(namespace), key),
        };
        let entry = lookup(namespace, key).ok_or_else(|| match namespace {
            None => unknown_bare_key(key),
            Some(ns) if !catalog().namespaces.contains_key(ns) => unknown_namespace(ns),
            Some(ns) => unknown_namespace_key(ns, key),
        })?;
        match &entry.apply {
            ApplyKind::Host(f) => f(&mut builder, &value)?,
            ApplyKind::Usage(f) => f(&mut builder.usage, &value)?,
            ApplyKind::MetadataWildcard => {
                builder.metadata.insert(key.to_string(), value);
            }
        }
    }
    Ok(builder.into_host())
}

pub fn parse_usage_filter(
    params: HashMap<String, String>,
) -> Result<UsageFilter, FilterParseError> {
    let mut builder = UsageFilterBuilder::default();
    for (raw_key, value) in params {
        let (namespace, key) = match split_filter_key(&raw_key)? {
            FilterKey::Bare(k) => return Err(unknown_bare_key(k)),
            FilterKey::Namespaced { namespace, key } => (Some(namespace), key),
        };
        let entry = lookup(namespace, key).ok_or_else(|| match namespace {
            None => unreachable!(),
            Some(ns) if !catalog().namespaces.contains_key(ns) => unknown_namespace(ns),
            Some(ns) => unknown_namespace_key(ns, key),
        })?;
        match &entry.apply {
            // Usage parser only accepts usage.* keys.
            ApplyKind::Usage(f) => f(&mut builder, &value)?,
            _ => return Err(unknown_namespace_key(namespace.unwrap_or(""), key)),
        }
    }
    Ok(builder.into_usage())
}

enum FilterKey<'a> {
    Bare(&'a str),
    Namespaced { namespace: &'a str, key: &'a str },
}

fn split_filter_key(key: &str) -> Result<FilterKey<'_>, FilterParseError> {
    let Some((namespace, child_key)) = key.split_once('.') else {
        return Ok(FilterKey::Bare(key));
    };
    if namespace.is_empty() || child_key.is_empty() || child_key.contains('.') {
        return Err(FilterParseError::InvalidKey {
            key: key.to_string(),
        });
    }
    Ok(FilterKey::Namespaced {
        namespace,
        key: child_key,
    })
}

fn parse_duration_query(name: &str, value: &str) -> Result<Duration, FilterParseError> {
    humantime::parse_duration(value).map_err(|err| invalid_value(name, value, err))
}

fn parse_icmp_status(name: &str, value: &str) -> Result<HostStatus, FilterParseError> {
    HostStatus::try_from(value).map_err(|err| invalid_value(name, value, err))
}

fn parse_usage_status(name: &str, value: &str) -> Result<UsageCollectionStatus, FilterParseError> {
    UsageCollectionStatus::try_from(value).map_err(|err| invalid_value(name, value, err))
}

fn invalid_value(name: &str, value: &str, message: impl fmt::Display) -> FilterParseError {
    FilterParseError::InvalidValue {
        name: name.to_string(),
        value: value.to_string(),
        message: message.to_string(),
    }
}

fn unknown_bare_key(key: &str) -> FilterParseError {
    FilterParseError::UnknownBareKey {
        key: key.to_string(),
        bare: known_bare_keys(),
        namespaces: known_namespaces(),
    }
}

fn unknown_namespace(namespace: &str) -> FilterParseError {
    FilterParseError::UnknownNamespace {
        namespace: namespace.to_string(),
        namespaces: known_namespaces(),
    }
}

fn unknown_namespace_key(namespace: &str, key: &str) -> FilterParseError {
    FilterParseError::UnknownNamespaceKey {
        namespace: namespace.to_string(),
        key: key.to_string(),
        valid: known_namespace_keys(namespace),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rstest::rstest;

    #[rstest]
    #[case("status", "broken")]
    #[case("disk.status", "full")]
    #[case("icmp.latency", "slow")]
    fn rejects_unknown_or_unnamespaced_host_filters(#[case] key: &str, #[case] value: &str) {
        let params = HashMap::from([(key.to_string(), value.to_string())]);
        assert!(parse_host_filter(params).is_err());
    }

    #[rstest]
    #[case("usage.no_users_for", "not-a-duration")]
    #[case("usage.status", "broken")]
    fn rejects_invalid_usage_filter_values(#[case] key: &str, #[case] value: &str) {
        let params = HashMap::from([(key.to_string(), value.to_string())]);
        assert!(parse_usage_filter(params).is_err());
    }

    #[test]
    fn parses_combined_host_filter() {
        let filter = parse_host_filter(HashMap::from([
            ("icmp.status".to_string(), "down".to_string()),
            ("group".to_string(), "core".to_string()),
            ("metadata.room".to_string(), "net-a".to_string()),
            ("usage.no_users_for".to_string(), "7days".to_string()),
        ]))
        .unwrap();

        assert_eq!(filter.icmp_status, Some(HostStatus::Down));
        assert_eq!(filter.group.as_deref(), Some("core"));
        assert_eq!(filter.metadata["room"], "net-a");
        assert!(filter.usage.no_users_for.is_some());
    }

    #[test]
    fn catalog_is_derived_from_registry() {
        // Adding/removing a REGISTRY entry must change /v1/namespaces in lockstep.
        let cat = namespace_catalog();
        assert!(cat.bare.contains_key("group"));
        assert!(cat.namespaces["icmp"].contains_key("status"));
        assert!(cat.namespaces["metadata"].contains_key("*"));
        assert!(cat.namespaces["usage"].contains_key("inactive_console_for"));
    }
}
