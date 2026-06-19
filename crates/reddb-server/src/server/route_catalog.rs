use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
pub(crate) enum RouteMethod {
    Get,
    Post,
    Put,
    Patch,
    Delete,
    Options,
    Head,
    Any,
}

impl RouteMethod {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
            Self::Options => "OPTIONS",
            Self::Head => "HEAD",
            Self::Any => "*",
        }
    }

    fn overlaps(self, other: Self) -> bool {
        self == Self::Any || other == Self::Any || self == other
    }
}

impl fmt::Display for RouteMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RouteAudience {
    Client,
    Operator,
    Infra,
    CompatibilityAdapter,
    Internal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RouteAuth {
    Public,
    OptionalUser,
    UserRequired,
    AdminToken,
    OpsCapability(&'static str),
    StreamLease,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ListenerSurface {
    Public,
    Admin,
    Metrics,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RouteStability {
    Stable,
    Compatibility,
    Deprecated,
    Internal,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RouteMiddleware {
    CorsPreflight,
    ListenerSurfaceGate,
    UiStaticAssetBypass,
    AuthGate,
    QuotaGate,
    QuotaBypass,
    OpsPolicy(&'static str),
    StreamingSlot,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RouteAlias {
    pub(crate) method: RouteMethod,
    pub(crate) pattern: &'static str,
    pub(crate) stability: RouteStability,
    pub(crate) note: &'static str,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RouteSpec {
    pub(crate) id: &'static str,
    pub(crate) method: RouteMethod,
    pub(crate) pattern: &'static str,
    pub(crate) family: &'static str,
    pub(crate) audience: RouteAudience,
    pub(crate) auth: RouteAuth,
    pub(crate) surfaces: &'static [ListenerSurface],
    pub(crate) stability: RouteStability,
    pub(crate) aliases: &'static [RouteAlias],
    pub(crate) middlewares: &'static [RouteMiddleware],
}

#[derive(Default)]
pub(crate) struct RouteRegistry {
    specs: Vec<RouteSpec>,
}

impl RouteRegistry {
    pub(crate) fn route(&mut self, spec: RouteSpec) {
        self.specs.push(spec);
    }

    pub(crate) fn into_specs(self) -> Vec<RouteSpec> {
        self.specs
    }
}

#[derive(Debug)]
pub(crate) struct RouteCatalog {
    routes: Vec<CompiledRoute>,
    exact_index: BTreeMap<(RouteMethod, &'static str), usize>,
    dynamic_indices: Vec<usize>,
}

impl RouteCatalog {
    pub(crate) fn build(specs: Vec<RouteSpec>) -> Result<Self, RouteCatalogError> {
        let mut seen_ids = BTreeSet::new();
        let mut routes = Vec::with_capacity(specs.len());

        for spec in specs {
            if !seen_ids.insert(spec.id) {
                return Err(RouteCatalogError::DuplicateRouteId { id: spec.id });
            }

            let pattern = RoutePattern::parse(spec.pattern).map_err(|reason| {
                RouteCatalogError::InvalidPattern {
                    route_id: spec.id,
                    pattern: spec.pattern,
                    reason,
                }
            })?;
            routes.push(CompiledRoute { spec, pattern });
        }

        for left in 0..routes.len() {
            for right in (left + 1)..routes.len() {
                let a = &routes[left];
                let b = &routes[right];
                if !a.spec.method.overlaps(b.spec.method) {
                    continue;
                }

                if a.pattern.raw == b.pattern.raw {
                    return Err(RouteCatalogError::DuplicateRoute {
                        method: a.spec.method,
                        pattern: a.pattern.raw,
                        first_id: a.spec.id,
                        second_id: b.spec.id,
                    });
                }

                if !a.pattern.is_exact() && !b.pattern.is_exact() && a.pattern.overlaps(&b.pattern)
                {
                    return Err(RouteCatalogError::AmbiguousDynamicRoutes {
                        method: a.spec.method,
                        first_id: a.spec.id,
                        first_pattern: a.pattern.raw,
                        second_id: b.spec.id,
                        second_pattern: b.pattern.raw,
                    });
                }
            }
        }

        let mut exact_index = BTreeMap::new();
        let mut dynamic_indices = Vec::new();
        for (index, route) in routes.iter().enumerate() {
            if route.pattern.is_exact() {
                exact_index.insert((route.spec.method, route.pattern.raw), index);
            } else {
                dynamic_indices.push(index);
            }
        }

        Ok(Self {
            routes,
            exact_index,
            dynamic_indices,
        })
    }

    pub(crate) fn routes(&self) -> impl Iterator<Item = &RouteSpec> {
        self.routes.iter().map(|route| &route.spec)
    }

    pub(crate) fn find(&self, method: RouteMethod, path: &str) -> Option<RouteMatch<'_>> {
        if let Some(index) = self.exact_index.get(&(method, path)) {
            return Some(RouteMatch {
                spec: &self.routes[*index].spec,
                params: BTreeMap::new(),
            });
        }

        if let Some(index) = self.exact_index.get(&(RouteMethod::Any, path)) {
            return Some(RouteMatch {
                spec: &self.routes[*index].spec,
                params: BTreeMap::new(),
            });
        }

        for index in &self.dynamic_indices {
            let route = &self.routes[*index];
            if !route.spec.method.overlaps(method) {
                continue;
            }
            if let Some(params) = route.pattern.matches(path) {
                return Some(RouteMatch {
                    spec: &route.spec,
                    params,
                });
            }
        }

        None
    }
}

pub(crate) struct RouteMatch<'a> {
    pub(crate) spec: &'a RouteSpec,
    pub(crate) params: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RouteCatalogError {
    InvalidPattern {
        route_id: &'static str,
        pattern: &'static str,
        reason: String,
    },
    DuplicateRouteId {
        id: &'static str,
    },
    DuplicateRoute {
        method: RouteMethod,
        pattern: &'static str,
        first_id: &'static str,
        second_id: &'static str,
    },
    AmbiguousDynamicRoutes {
        method: RouteMethod,
        first_id: &'static str,
        first_pattern: &'static str,
        second_id: &'static str,
        second_pattern: &'static str,
    },
}

impl fmt::Display for RouteCatalogError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidPattern {
                route_id,
                pattern,
                reason,
            } => write!(f, "route {route_id} has invalid pattern {pattern}: {reason}"),
            Self::DuplicateRouteId { id } => write!(f, "duplicate route id {id}"),
            Self::DuplicateRoute {
                method,
                pattern,
                first_id,
                second_id,
            } => write!(
                f,
                "duplicate route {method} {pattern}: {first_id} conflicts with {second_id}"
            ),
            Self::AmbiguousDynamicRoutes {
                method,
                first_id,
                first_pattern,
                second_id,
                second_pattern,
            } => write!(
                f,
                "ambiguous dynamic route for {method}: {first_id} {first_pattern} overlaps {second_id} {second_pattern}"
            ),
        }
    }
}

impl std::error::Error for RouteCatalogError {}

#[derive(Clone, Debug)]
struct CompiledRoute {
    spec: RouteSpec,
    pattern: RoutePattern,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct RoutePattern {
    raw: &'static str,
    segments: Vec<RouteSegment>,
}

impl RoutePattern {
    fn parse(raw: &'static str) -> Result<Self, String> {
        if !raw.starts_with('/') && raw != "*" {
            return Err("patterns must start with '/' or be '*'".to_string());
        }
        if raw == "*" {
            return Ok(Self {
                raw,
                segments: vec![RouteSegment::Wildcard],
            });
        }
        if raw == "/" {
            return Ok(Self {
                raw,
                segments: Vec::new(),
            });
        }

        let parts: Vec<&str> = raw[1..].split('/').collect();
        let mut segments = Vec::with_capacity(parts.len());
        for (index, part) in parts.iter().enumerate() {
            if part.is_empty() && index + 1 != parts.len() {
                return Err("empty interior path segments are not supported".to_string());
            }
            if let Some(name) = part.strip_prefix(':') {
                if name.is_empty() {
                    return Err("path parameter name cannot be empty".to_string());
                }
                if name.ends_with('?') {
                    return Err(
                        "optional path parameters are intentionally unsupported; register both explicit routes"
                            .to_string(),
                    );
                }
                segments.push(RouteSegment::Param(name.to_string()));
            } else if *part == "*" {
                if index + 1 != parts.len() {
                    return Err("wildcards must be terminal path segments".to_string());
                }
                segments.push(RouteSegment::Wildcard);
            } else {
                segments.push(RouteSegment::Static((*part).to_string()));
            }
        }

        Ok(Self { raw, segments })
    }

    fn is_exact(&self) -> bool {
        self.segments
            .iter()
            .all(|segment| matches!(segment, RouteSegment::Static(_)))
    }

    fn matches(&self, path: &str) -> Option<BTreeMap<String, String>> {
        if !path.starts_with('/') {
            return None;
        }
        if self.raw == "*" {
            let mut params = BTreeMap::new();
            if path != "/" {
                params.insert("*".to_string(), path.trim_start_matches('/').to_string());
            }
            return Some(params);
        }

        let parts = split_request_path(path)?;
        let mut params = BTreeMap::new();
        let mut part_index = 0;

        for segment in &self.segments {
            match segment {
                RouteSegment::Static(expected) => {
                    if parts.get(part_index)? != expected {
                        return None;
                    }
                    part_index += 1;
                }
                RouteSegment::Param(name) => {
                    let value = parts.get(part_index)?;
                    if value.is_empty() {
                        return None;
                    }
                    params.insert(name.clone(), (*value).to_string());
                    part_index += 1;
                }
                RouteSegment::Wildcard => {
                    if part_index < parts.len() {
                        params.insert("*".to_string(), parts[part_index..].join("/"));
                    }
                    return Some(params);
                }
            }
        }

        if part_index == parts.len() {
            Some(params)
        } else {
            None
        }
    }

    fn overlaps(&self, other: &Self) -> bool {
        let mut index = 0;
        loop {
            let left = self.segments.get(index);
            let right = other.segments.get(index);
            match (left, right) {
                (None, None) => return true,
                (Some(RouteSegment::Wildcard), _) | (_, Some(RouteSegment::Wildcard)) => {
                    return true;
                }
                (None, Some(_)) | (Some(_), None) => return false,
                (
                    Some(RouteSegment::Static(left_static)),
                    Some(RouteSegment::Static(right_static)),
                ) if left_static != right_static => return false,
                _ => index += 1,
            }
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum RouteSegment {
    Static(String),
    Param(String),
    Wildcard,
}

fn split_request_path(path: &str) -> Option<Vec<&str>> {
    if !path.starts_with('/') {
        return None;
    }
    if path == "/" {
        return Some(Vec::new());
    }
    Some(path[1..].split('/').collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    const NO_SURFACES: &[ListenerSurface] = &[ListenerSurface::Public];
    const NO_MIDDLEWARE: &[RouteMiddleware] = &[];

    fn spec(id: &'static str, method: RouteMethod, pattern: &'static str) -> RouteSpec {
        RouteSpec {
            id,
            method,
            pattern,
            family: "test",
            audience: RouteAudience::Client,
            auth: RouteAuth::UserRequired,
            surfaces: NO_SURFACES,
            stability: RouteStability::Stable,
            aliases: &[],
            middlewares: NO_MIDDLEWARE,
        }
    }

    #[test]
    fn exact_routes_match_before_dynamic_routes() {
        let catalog = RouteCatalog::build(vec![
            spec("collection.by_name", RouteMethod::Get, "/collections/:name"),
            spec("collection.new", RouteMethod::Get, "/collections/new"),
        ])
        .unwrap();

        let matched = catalog.find(RouteMethod::Get, "/collections/new").unwrap();

        assert_eq!(matched.spec.id, "collection.new");
        assert!(matched.params.is_empty());
    }

    #[test]
    fn dynamic_routes_capture_named_params() {
        let catalog = RouteCatalog::build(vec![spec(
            "collection.by_name",
            RouteMethod::Get,
            "/collections/:name",
        )])
        .unwrap();

        let matched = catalog
            .find(RouteMethod::Get, "/collections/users")
            .unwrap();

        assert_eq!(matched.spec.id, "collection.by_name");
        assert_eq!(
            matched.params.get("name").map(String::as_str),
            Some("users")
        );
    }

    #[test]
    fn dynamic_params_do_not_match_empty_segments() {
        let catalog = RouteCatalog::build(vec![spec(
            "collection.by_name",
            RouteMethod::Get,
            "/collections/:name",
        )])
        .unwrap();

        assert!(catalog.find(RouteMethod::Get, "/collections/").is_none());
    }

    #[test]
    fn duplicate_exact_routes_are_rejected() {
        let err = RouteCatalog::build(vec![
            spec("first", RouteMethod::Get, "/health/live"),
            spec("second", RouteMethod::Get, "/health/live"),
        ])
        .unwrap_err();

        assert!(matches!(err, RouteCatalogError::DuplicateRoute { .. }));
    }

    #[test]
    fn overlapping_dynamic_routes_are_rejected() {
        let err = RouteCatalog::build(vec![
            spec("generic", RouteMethod::Get, "/:family/:id"),
            spec("collections", RouteMethod::Get, "/collections/:name"),
        ])
        .unwrap_err();

        assert!(matches!(
            err,
            RouteCatalogError::AmbiguousDynamicRoutes { .. }
        ));
    }

    #[test]
    fn optional_params_are_rejected() {
        let err = RouteCatalog::build(vec![spec("optional", RouteMethod::Get, "/users/:id?")])
            .unwrap_err();

        assert!(matches!(err, RouteCatalogError::InvalidPattern { .. }));
        assert!(err.to_string().contains("optional path parameters"));
    }
}
