use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::Arc;

use crate::application::OperationContext;
use crate::auth::policies::{self, EvalContext, Policy, ResourceRef};

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
    pub(crate) fn from_http_method(method: &str) -> Option<Self> {
        match method {
            "GET" => Some(Self::Get),
            "POST" => Some(Self::Post),
            "PUT" => Some(Self::Put),
            "PATCH" => Some(Self::Patch),
            "DELETE" => Some(Self::Delete),
            "OPTIONS" => Some(Self::Options),
            "HEAD" => Some(Self::Head),
            "*" => Some(Self::Any),
            _ => None,
        }
    }

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
pub(crate) enum CommandAudience {
    Client,
    Operator,
    Infra,
    CompatibilityAdapter,
    Internal,
}

impl CommandAudience {
    fn as_str(self) -> &'static str {
        match self {
            Self::Client => "client",
            Self::Operator => "operator",
            Self::Infra => "infra",
            Self::CompatibilityAdapter => "compatibility-adapter",
            Self::Internal => "internal",
        }
    }
}

pub(crate) type RouteAudience = CommandAudience;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommandAuthRequirement {
    Public,
    OptionalUser,
    UserRequired,
    AdminToken,
    OpsCapability(&'static str),
    StreamLease,
}

pub(crate) type RouteAuth = CommandAuthRequirement;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum ListenerSurface {
    Public,
    Admin,
    Metrics,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommandStability {
    Stable,
    Compatibility,
    Deprecated,
    Internal,
}

pub(crate) type RouteStability = CommandStability;

/// Transport-neutral payload shape carried by one command declaration.
///
/// Dispatchers may translate these shapes into HTTP bodies, protobuf
/// messages, MCP values, or wire frames. `Unspecified` is accepted by the
/// Rust type so catalog construction can report a useful error instead of
/// forcing every declaration through a macro.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CommandShape {
    Unspecified,
    Empty,
    Structured,
    Stream,
}

impl CommandShape {
    pub(crate) const fn is_declared(self) -> bool {
        !matches!(self, Self::Unspecified)
    }
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

impl RouteAlias {
    pub(crate) const fn canonical(
        method: RouteMethod,
        pattern: &'static str,
        note: &'static str,
    ) -> Self {
        Self {
            method,
            pattern,
            stability: RouteStability::Stable,
            note,
        }
    }

    pub(crate) const fn compatibility(
        method: RouteMethod,
        pattern: &'static str,
        note: &'static str,
    ) -> Self {
        Self {
            method,
            pattern,
            stability: RouteStability::Compatibility,
            note,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CommandSpec {
    pub(crate) id: &'static str,
    pub(crate) input_shape: CommandShape,
    pub(crate) output_shape: CommandShape,
    pub(crate) method: RouteMethod,
    pub(crate) pattern: &'static str,
    pub(crate) family: &'static str,
    pub(crate) audience: CommandAudience,
    pub(crate) auth: CommandAuthRequirement,
    pub(crate) surfaces: &'static [ListenerSurface],
    pub(crate) stability: CommandStability,
    pub(crate) aliases: &'static [RouteAlias],
    pub(crate) middlewares: &'static [RouteMiddleware],
}

pub(crate) type RouteSpec = CommandSpec;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RouteGroupDefaults {
    pub(crate) family: &'static str,
    pub(crate) audience: RouteAudience,
    pub(crate) auth: RouteAuth,
    pub(crate) surfaces: &'static [ListenerSurface],
    pub(crate) stability: RouteStability,
    pub(crate) middlewares: &'static [RouteMiddleware],
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct RouteEntry {
    pub(crate) id: &'static str,
    pub(crate) input_shape: CommandShape,
    pub(crate) output_shape: CommandShape,
    pub(crate) method: RouteMethod,
    pub(crate) pattern: &'static str,
    pub(crate) aliases: &'static [RouteAlias],
}

impl RouteEntry {
    pub(crate) const fn new(id: &'static str, method: RouteMethod, pattern: &'static str) -> Self {
        Self {
            id,
            input_shape: CommandShape::Structured,
            output_shape: CommandShape::Structured,
            method,
            pattern,
            aliases: &[],
        }
    }

    pub(crate) const fn with_aliases(
        id: &'static str,
        method: RouteMethod,
        pattern: &'static str,
        aliases: &'static [RouteAlias],
    ) -> Self {
        Self {
            id,
            input_shape: CommandShape::Structured,
            output_shape: CommandShape::Structured,
            method,
            pattern,
            aliases,
        }
    }
}

#[derive(Default)]
pub(crate) struct RouteRegistry {
    specs: Vec<RouteSpec>,
}

impl RouteRegistry {
    pub(crate) fn route(&mut self, spec: RouteSpec) {
        self.specs.push(spec);
    }

    pub(crate) fn routes(&mut self, defaults: RouteGroupDefaults, entries: &[RouteEntry]) {
        for entry in entries {
            self.route(RouteSpec {
                id: entry.id,
                input_shape: entry.input_shape,
                output_shape: entry.output_shape,
                method: entry.method,
                pattern: entry.pattern,
                family: defaults.family,
                audience: defaults.audience,
                auth: defaults.auth,
                surfaces: defaults.surfaces,
                stability: defaults.stability,
                aliases: entry.aliases,
                middlewares: defaults.middlewares,
            });
        }
    }

    pub(crate) fn into_specs(self) -> Vec<RouteSpec> {
        self.specs
    }
}

#[derive(Debug)]
pub(crate) struct CommandCatalog {
    routes: Vec<CompiledRoute>,
    command_index: BTreeMap<&'static str, usize>,
    exact_index: BTreeMap<(RouteMethod, &'static str), usize>,
    dynamic_indices: Vec<usize>,
    aliases: Vec<CompiledAlias>,
}

impl CommandCatalog {
    pub(crate) fn build(specs: Vec<RouteSpec>) -> Result<Self, RouteCatalogError> {
        let mut seen_ids = BTreeSet::new();
        let mut routes = Vec::with_capacity(specs.len());

        for spec in specs {
            if !seen_ids.insert(spec.id) {
                return Err(RouteCatalogError::DuplicateRouteId { id: spec.id });
            }
            if !spec.input_shape.is_declared() || !spec.output_shape.is_declared() {
                return Err(RouteCatalogError::UndeclaredCommandShape { id: spec.id });
            }

            let pattern = RoutePattern::parse(spec.pattern).map_err(|reason| {
                RouteCatalogError::InvalidPattern {
                    route_id: spec.id,
                    pattern: spec.pattern,
                    reason,
                }
            })?;
            let mut aliases = Vec::with_capacity(spec.aliases.len());
            for alias in spec.aliases {
                let pattern = RoutePattern::parse(alias.pattern).map_err(|reason| {
                    RouteCatalogError::InvalidPattern {
                        route_id: spec.id,
                        pattern: alias.pattern,
                        reason,
                    }
                })?;
                aliases.push(CompiledRouteAlias {
                    alias: *alias,
                    pattern,
                });
            }
            routes.push(CompiledRoute {
                spec,
                pattern,
                aliases,
            });
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
        let mut command_index = BTreeMap::new();
        let mut dynamic_indices = Vec::new();
        let mut aliases = Vec::new();
        for (index, route) in routes.iter().enumerate() {
            command_index.insert(route.spec.id, index);
            if route.pattern.is_exact() {
                exact_index.insert((route.spec.method, route.pattern.raw), index);
            } else {
                dynamic_indices.push(index);
            }
            for alias in &route.aliases {
                aliases.push(CompiledAlias {
                    route_index: index,
                    alias: alias.alias,
                    pattern: alias.pattern.clone(),
                });
            }
        }

        Ok(Self {
            routes,
            command_index,
            exact_index,
            dynamic_indices,
            aliases,
        })
    }

    pub(crate) fn routes(&self) -> impl Iterator<Item = &RouteSpec> {
        self.routes.iter().map(|route| &route.spec)
    }

    pub(crate) fn commands(&self) -> impl ExactSizeIterator<Item = &CommandSpec> {
        self.routes.iter().map(|route| &route.spec)
    }

    pub(crate) fn command(&self, id: &str) -> Option<&CommandSpec> {
        self.command_index
            .get(id)
            .map(|index| &self.routes[*index].spec)
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

        if self
            .exact_index
            .keys()
            .any(|(_, exact_path)| *exact_path == path)
        {
            return None;
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

    pub(crate) fn path_exists(&self, path: &str) -> bool {
        if self
            .exact_index
            .keys()
            .any(|(_, exact_path)| *exact_path == path)
        {
            return true;
        }

        self.dynamic_indices
            .iter()
            .any(|index| self.routes[*index].pattern.matches(path).is_some())
            || self
                .aliases
                .iter()
                .any(|alias| alias.pattern.matches(path).is_some())
    }

    pub(crate) fn resolve_alias(&self, method: RouteMethod, path: &str) -> Option<String> {
        for alias in &self.aliases {
            if !alias.alias.method.overlaps(method) {
                continue;
            }
            let Some(params) = alias.pattern.matches(path) else {
                continue;
            };
            let route = &self.routes[alias.route_index];
            return route.pattern.render(&params);
        }

        None
    }
}

pub(crate) type RouteCatalog = CommandCatalog;

/// Boundary to the ADR 0021 policy engine. Dispatchers supply a context and
/// command id to [`CommandAuthorizer`]; this is the only seam that receives
/// the command's authentication requirement and audience.
pub(crate) trait CommandPolicyEngine {
    fn allows(&self, ctx: &OperationContext, command: &CommandSpec) -> bool;
}

/// Adapter from command declarations to the ADR 0021 IAM evaluator.
pub(crate) struct IamCommandPolicyEngine<'a> {
    policies: &'a [Arc<Policy>],
}

impl<'a> IamCommandPolicyEngine<'a> {
    pub(crate) fn new(policies: &'a [Arc<Policy>]) -> Self {
        Self { policies }
    }
}

impl CommandPolicyEngine for IamCommandPolicyEngine<'_> {
    fn allows(&self, ctx: &OperationContext, command: &CommandSpec) -> bool {
        let Some(action) = command_policy_action(command) else {
            return true;
        };
        if ctx.audit_principal == "anonymous" {
            return false;
        }

        let resource_name = format!("{}:{}", command.audience.as_str(), command.id);
        let mut resource = ResourceRef::new("command", resource_name);
        if let Some(tenant) = &ctx.tenant {
            resource = resource.with_tenant(tenant);
        }
        let eval_context = EvalContext {
            principal_tenant: ctx.tenant.clone(),
            current_tenant: ctx.tenant.clone(),
            now_ms: crate::auth::now_ms(),
            principal_is_platform_scoped: ctx.tenant.is_none(),
            ..EvalContext::default()
        };
        let policies: Vec<&Policy> = self.policies.iter().map(Arc::as_ref).collect();

        matches!(
            policies::evaluate(&policies, action, &resource, &eval_context),
            policies::Decision::Allow { .. } | policies::Decision::AdminBypass
        )
    }
}

fn command_policy_action(command: &CommandSpec) -> Option<&'static str> {
    match command.auth {
        RouteAuth::Public | RouteAuth::OptionalUser => None,
        RouteAuth::UserRequired => match command.method {
            RouteMethod::Get | RouteMethod::Options | RouteMethod::Head => Some("select"),
            RouteMethod::Post
            | RouteMethod::Put
            | RouteMethod::Patch
            | RouteMethod::Delete
            | RouteMethod::Any => Some("write"),
        },
        RouteAuth::AdminToken => Some("admin:*"),
        RouteAuth::OpsCapability(action) => Some(action),
        RouteAuth::StreamLease => Some("stream"),
    }
}

pub(crate) struct CommandAuthorizer<'a, P: ?Sized> {
    catalog: &'a CommandCatalog,
    policy_engine: &'a P,
}

impl<'a, P: CommandPolicyEngine + ?Sized> CommandAuthorizer<'a, P> {
    pub(crate) fn new(catalog: &'a CommandCatalog, policy_engine: &'a P) -> Self {
        Self {
            catalog,
            policy_engine,
        }
    }

    pub(crate) fn authorize(
        &self,
        ctx: &OperationContext,
        command_id: &str,
    ) -> Result<(), CommandAuthorizationError> {
        let command = self
            .catalog
            .command(command_id)
            .ok_or_else(|| CommandAuthorizationError::UnknownCommand(command_id.to_string()))?;
        if self.policy_engine.allows(ctx, command) {
            Ok(())
        } else {
            Err(CommandAuthorizationError::Denied {
                command_id: command.id,
            })
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum CommandAuthorizationError {
    UnknownCommand(String),
    Denied { command_id: &'static str },
}

impl fmt::Display for CommandAuthorizationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownCommand(id) => write!(f, "unknown command {id}"),
            Self::Denied { command_id } => write!(f, "command {command_id} denied by policy"),
        }
    }
}

impl std::error::Error for CommandAuthorizationError {}

pub(crate) struct RouteMatch<'a> {
    pub(crate) spec: &'a RouteSpec,
    pub(crate) params: BTreeMap<String, String>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum RouteCatalogError {
    UndeclaredCommandShape {
        id: &'static str,
    },
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
            Self::UndeclaredCommandShape { id } => {
                write!(f, "command {id} has an undeclared input or output shape")
            }
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
    aliases: Vec<CompiledRouteAlias>,
}

#[derive(Clone, Debug)]
struct CompiledRouteAlias {
    alias: RouteAlias,
    pattern: RoutePattern,
}

#[derive(Clone, Debug)]
struct CompiledAlias {
    route_index: usize,
    alias: RouteAlias,
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

    fn render(&self, params: &BTreeMap<String, String>) -> Option<String> {
        if self.raw == "*" {
            return Some(
                params
                    .get("*")
                    .map(|value| format!("/{value}"))
                    .unwrap_or_else(|| "/".to_string()),
            );
        }
        if self.segments.is_empty() {
            return Some("/".to_string());
        }

        let mut rendered = String::new();
        for segment in &self.segments {
            rendered.push('/');
            match segment {
                RouteSegment::Static(value) => rendered.push_str(value),
                RouteSegment::Param(name) => rendered.push_str(params.get(name)?),
                RouteSegment::Wildcard => {
                    if let Some(value) = params.get("*") {
                        rendered.push_str(value);
                    }
                }
            }
        }
        Some(rendered)
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
    use crate::application::OperationContext;
    use crate::auth::policies::Policy;
    use std::sync::Arc;

    const NO_SURFACES: &[ListenerSurface] = &[ListenerSurface::Public];
    const NO_MIDDLEWARE: &[RouteMiddleware] = &[];

    fn spec(id: &'static str, method: RouteMethod, pattern: &'static str) -> RouteSpec {
        RouteSpec {
            id,
            input_shape: CommandShape::Structured,
            output_shape: CommandShape::Structured,
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

    fn spec_with_alias(
        id: &'static str,
        method: RouteMethod,
        pattern: &'static str,
        aliases: &'static [RouteAlias],
    ) -> RouteSpec {
        RouteSpec {
            aliases,
            ..spec(id, method, pattern)
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
    fn exact_path_in_other_method_blocks_dynamic_match() {
        let catalog = RouteCatalog::build(vec![
            spec("policy.lint", RouteMethod::Post, "/admin/policies/lint"),
            spec("policy.get", RouteMethod::Get, "/admin/policies/:id"),
        ])
        .unwrap();

        assert!(catalog
            .find(RouteMethod::Get, "/admin/policies/lint")
            .is_none());
        assert!(catalog.path_exists("/admin/policies/lint"));
    }

    #[test]
    fn path_exists_matches_dynamic_routes() {
        let catalog = RouteCatalog::build(vec![spec(
            "policy.get",
            RouteMethod::Get,
            "/admin/policies/:id",
        )])
        .unwrap();

        assert!(catalog.path_exists("/admin/policies/p1"));
        assert!(!catalog.path_exists("/admin/policies/p1/extra"));
    }

    #[test]
    fn path_exists_matches_alias_routes() {
        const ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
            RouteMethod::Post,
            "/v1/admin/policies/lint",
            "canonical policy lint path",
        )];
        let catalog = RouteCatalog::build(vec![spec_with_alias(
            "policy.lint",
            RouteMethod::Post,
            "/admin/policies/lint",
            ALIASES,
        )])
        .unwrap();

        assert!(catalog.path_exists("/v1/admin/policies/lint"));
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

    #[test]
    fn exact_aliases_resolve_to_live_route_path() {
        const ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
            RouteMethod::Post,
            "/v1/auth/login",
            "canonical auth path",
        )];
        let catalog = RouteCatalog::build(vec![spec_with_alias(
            "auth.login",
            RouteMethod::Post,
            "/auth/login",
            ALIASES,
        )])
        .unwrap();

        assert_eq!(
            catalog.resolve_alias(RouteMethod::Post, "/v1/auth/login"),
            Some("/auth/login".to_string())
        );
    }

    #[test]
    fn dynamic_aliases_resolve_to_live_route_path_with_params() {
        const ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
            RouteMethod::Get,
            "/v1/ai/models/:name",
            "canonical AI model path",
        )];
        let catalog = RouteCatalog::build(vec![spec_with_alias(
            "ai.models.get",
            RouteMethod::Get,
            "/ai/models/:name",
            ALIASES,
        )])
        .unwrap();

        assert_eq!(
            catalog.resolve_alias(RouteMethod::Get, "/v1/ai/models/embedding-small"),
            Some("/ai/models/embedding-small".to_string())
        );
    }

    struct MatrixPolicyEngine {
        allowed_auth: RouteAuth,
        allowed_audience: RouteAudience,
    }

    impl CommandPolicyEngine for MatrixPolicyEngine {
        fn allows(&self, _ctx: &OperationContext, command: &CommandSpec) -> bool {
            command.auth == self.allowed_auth && command.audience == self.allowed_audience
        }
    }

    #[test]
    fn authorize_allows_and_denies_every_auth_requirement() {
        for auth in [
            RouteAuth::Public,
            RouteAuth::OptionalUser,
            RouteAuth::UserRequired,
            RouteAuth::AdminToken,
            RouteAuth::OpsCapability("ops:read:cluster"),
            RouteAuth::StreamLease,
        ] {
            let catalog = CommandCatalog::build(vec![RouteSpec {
                auth,
                ..spec("test.command", RouteMethod::Post, "/test")
            }])
            .expect("command catalog");
            let ctx = OperationContext::read_only("request-1");

            let allow = MatrixPolicyEngine {
                allowed_auth: auth,
                allowed_audience: RouteAudience::Client,
            };
            assert!(CommandAuthorizer::new(&catalog, &allow)
                .authorize(&ctx, "test.command")
                .is_ok());

            let deny = MatrixPolicyEngine {
                allowed_auth: if auth == RouteAuth::Public {
                    RouteAuth::UserRequired
                } else {
                    RouteAuth::Public
                },
                allowed_audience: RouteAudience::Client,
            };
            assert!(matches!(
                CommandAuthorizer::new(&catalog, &deny).authorize(&ctx, "test.command"),
                Err(CommandAuthorizationError::Denied { .. })
            ));
        }
    }

    #[test]
    fn authorize_allows_and_denies_every_command_audience() {
        for audience in [
            RouteAudience::Client,
            RouteAudience::Operator,
            RouteAudience::Infra,
            RouteAudience::CompatibilityAdapter,
            RouteAudience::Internal,
        ] {
            let catalog = CommandCatalog::build(vec![RouteSpec {
                audience,
                ..spec("test.command", RouteMethod::Post, "/test")
            }])
            .expect("command catalog");
            let ctx = OperationContext::read_only("request-1");

            let allow = MatrixPolicyEngine {
                allowed_auth: RouteAuth::UserRequired,
                allowed_audience: audience,
            };
            assert!(CommandAuthorizer::new(&catalog, &allow)
                .authorize(&ctx, "test.command")
                .is_ok());

            let deny = MatrixPolicyEngine {
                allowed_auth: RouteAuth::UserRequired,
                allowed_audience: RouteAudience::Internal,
            };
            if audience != RouteAudience::Internal {
                assert!(matches!(
                    CommandAuthorizer::new(&catalog, &deny).authorize(&ctx, "test.command"),
                    Err(CommandAuthorizationError::Denied { .. })
                ));
            }
        }
    }

    #[test]
    fn iam_policy_engine_applies_explicit_deny_and_audience_resource() {
        let allow = Arc::new(
            Policy::from_json_str(
                r#"{"id":"allow-operator","version":1,"statements":[
                    {"effect":"allow","actions":["write"],
                     "resources":["command:operator:test.command"]}
                ]}"#,
            )
            .expect("allow policy"),
        );
        let deny = Arc::new(
            Policy::from_json_str(
                r#"{"id":"deny-command","version":1,"statements":[
                    {"effect":"deny","actions":["write"],
                     "resources":["command:operator:test.command"]}
                ]}"#,
            )
            .expect("deny policy"),
        );
        let catalog = CommandCatalog::build(vec![RouteSpec {
            audience: RouteAudience::Operator,
            ..spec("test.command", RouteMethod::Post, "/test")
        }])
        .expect("command catalog");
        let ctx = OperationContext::read_only("request-1").with_principal("operator");

        let allow_policies = vec![allow.clone()];
        let allow_engine = IamCommandPolicyEngine::new(&allow_policies);
        assert!(CommandAuthorizer::new(&catalog, &allow_engine)
            .authorize(&ctx, "test.command")
            .is_ok());

        let deny_policies = vec![allow, deny];
        let deny_engine = IamCommandPolicyEngine::new(&deny_policies);
        assert!(matches!(
            CommandAuthorizer::new(&catalog, &deny_engine).authorize(&ctx, "test.command"),
            Err(CommandAuthorizationError::Denied { .. })
        ));
    }
}
