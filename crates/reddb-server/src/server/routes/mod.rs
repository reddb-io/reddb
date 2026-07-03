use super::route_catalog::{RouteCatalog, RouteCatalogError, RouteRegistry};
use std::sync::OnceLock;

pub(crate) mod common;

mod generated {
    include!(concat!(env!("OUT_DIR"), "/route_discovery.rs"));
}

static DISCOVERED_ROUTE_CATALOG: OnceLock<RouteCatalog> = OnceLock::new();

pub(crate) fn discovered_route_catalog() -> &'static RouteCatalog {
    DISCOVERED_ROUTE_CATALOG.get_or_init(|| {
        build_discovered_route_catalog().expect("discovered HTTP route catalog is valid")
    })
}

fn build_discovered_route_catalog() -> Result<RouteCatalog, RouteCatalogError> {
    let mut registry = RouteRegistry::default();
    generated::register_discovered_routes(&mut registry);
    RouteCatalog::build(registry.into_specs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::route_catalog::{
        RouteAudience, RouteMethod, RouteMiddleware, RouteStability,
    };

    #[test]
    fn build_time_discovery_registers_route_files() {
        let catalog = build_discovered_route_catalog().unwrap();
        let route_ids: Vec<&str> = catalog.routes().map(|route| route.id).collect();

        assert!(route_ids.contains(&"health.live"));
        assert!(route_ids.contains(&"health.ready"));
        assert!(route_ids.contains(&"health.startup"));
        assert!(route_ids.contains(&"auth.login"));
        assert!(route_ids.contains(&"query.execute"));
        assert!(route_ids.contains(&"streams.input"));
        assert!(route_ids.contains(&"metrics.scrape"));
        assert!(route_ids.contains(&"prometheus.query.get"));
        assert!(route_ids.contains(&"ai.models.get"));
        assert!(route_ids.contains(&"admin.shutdown"));
        assert!(route_ids.contains(&"admin.policies.get"));
        assert!(route_ids.contains(&"ops.cluster.status"));
        assert!(route_ids.contains(&"catalog.snapshot"));
        assert!(route_ids.contains(&"graph.neighborhood"));
        assert!(route_ids.contains(&"physical.metadata"));
    }

    #[test]
    fn discovered_routes_are_matchable() {
        let catalog = build_discovered_route_catalog().unwrap();
        let matched = catalog.find(RouteMethod::Get, "/health/live").unwrap();

        assert_eq!(matched.spec.id, "health.live");
    }

    #[test]
    fn discovered_ec_dynamic_routes_resolve_through_catalog() {
        let catalog = build_discovered_route_catalog().unwrap();

        for (method, path, id) in [
            (RouteMethod::Post, "/ec/orders/total/add", "ops.ec.add"),
            (RouteMethod::Post, "/ec/orders/total/sub", "ops.ec.sub"),
            (RouteMethod::Post, "/ec/orders/total/set", "ops.ec.set"),
            (
                RouteMethod::Post,
                "/ec/orders/total/consolidate",
                "ops.ec.consolidate",
            ),
            (
                RouteMethod::Get,
                "/ec/orders/total/status",
                "ops.ec.field_status",
            ),
        ] {
            let matched = catalog
                .find(method, path)
                .unwrap_or_else(|| panic!("missing EC route {path}"));
            assert_eq!(matched.spec.id, id, "unexpected id for {path}");
            assert_eq!(
                matched.params.get("collection").map(String::as_str),
                Some("orders")
            );
            assert_eq!(
                matched.params.get("field").map(String::as_str),
                Some("total")
            );
        }

        // The global `/ec/status` exact route still wins over the dynamic
        // `/ec/:collection/:field/status` pattern.
        let global = catalog.find(RouteMethod::Get, "/ec/status").unwrap();
        assert_eq!(global.spec.id, "ops.ec.status");
    }

    #[test]
    fn discovered_geo_routes_resolve_through_catalog() {
        let catalog = build_discovered_route_catalog().unwrap();
        for (path, id) in [
            ("/geo/distance", "geo.distance"),
            ("/geo/bearing", "geo.bearing"),
            ("/geo/midpoint", "geo.midpoint"),
            ("/geo/destination", "geo.destination"),
            ("/geo/bounding-box", "geo.bounding_box"),
        ] {
            let matched = catalog
                .find(RouteMethod::Post, path)
                .unwrap_or_else(|| panic!("missing geo route {path}"));
            assert_eq!(matched.spec.id, id, "unexpected id for {path}");
        }
    }

    #[test]
    fn discovered_collection_entity_routes_resolve_with_params() {
        let catalog = build_discovered_route_catalog().unwrap();
        for (method, id) in [
            (RouteMethod::Get, "collections.entities.get"),
            (RouteMethod::Patch, "collections.entities.patch"),
            (RouteMethod::Put, "collections.entities.put"),
            (RouteMethod::Delete, "collections.entities.delete"),
        ] {
            let matched = catalog
                .find(method, "/collections/orders/entities/42")
                .unwrap_or_else(|| panic!("missing entity route for {id}"));
            assert_eq!(matched.spec.id, id);
            assert_eq!(
                matched.params.get("collection").map(String::as_str),
                Some("orders")
            );
            assert_eq!(matched.params.get("id").map(String::as_str), Some("42"));
        }
    }

    #[test]
    fn discovered_collection_dynamic_routes_do_not_shadow_each_other() {
        let catalog = build_discovered_route_catalog().unwrap();

        // The bare `DELETE /collections/:collection` DDL arm must not swallow
        // the more-specific entity / tree delete patterns.
        let bare = catalog
            .find(RouteMethod::Delete, "/collections/orders")
            .expect("bare collection drop resolves");
        assert_eq!(bare.spec.id, "collections.drop");
        assert_eq!(
            bare.params.get("collection").map(String::as_str),
            Some("orders")
        );

        let entity = catalog
            .find(RouteMethod::Delete, "/collections/orders/entities/7")
            .expect("entity delete resolves");
        assert_eq!(entity.spec.id, "collections.entities.delete");

        let tree_drop = catalog
            .find(RouteMethod::Delete, "/collections/orders/trees/hierarchy")
            .expect("tree drop resolves");
        assert_eq!(tree_drop.spec.id, "collections.trees.drop");
        assert_eq!(
            tree_drop.params.get("tree").map(String::as_str),
            Some("hierarchy")
        );

        let tree_node = catalog
            .find(
                RouteMethod::Delete,
                "/collections/orders/trees/hierarchy/nodes/3",
            )
            .expect("tree node delete resolves");
        assert_eq!(tree_node.spec.id, "collections.trees.nodes.delete");
        assert_eq!(tree_node.params.get("node").map(String::as_str), Some("3"));

        // The exact `/collections` listing arm wins over any dynamic
        // `/collections/:collection` pattern.
        let list = catalog.find(RouteMethod::Get, "/collections").unwrap();
        assert_eq!(list.spec.id, "collections.list");

        // Per-collection action arms remain distinct from the bulk variants.
        let documents = catalog
            .find(RouteMethod::Post, "/collections/orders/documents")
            .expect("documents create resolves");
        assert_eq!(documents.spec.id, "collections.documents.create");
        let bulk_documents = catalog
            .find(RouteMethod::Post, "/collections/orders/bulk/documents")
            .expect("bulk documents create resolves");
        assert_eq!(bulk_documents.spec.id, "collections.bulk.documents");
    }

    #[test]
    fn discovered_routes_carry_canonical_aliases() {
        let catalog = build_discovered_route_catalog().unwrap();

        let auth_login = catalog
            .routes()
            .find(|route| route.id == "auth.login")
            .expect("auth.login route is discovered");
        assert!(auth_login
            .aliases
            .iter()
            .any(|alias| alias.pattern == "/v1/auth/login"));

        let query = catalog
            .routes()
            .find(|route| route.id == "query.execute")
            .expect("query.execute route is discovered");
        assert!(query
            .aliases
            .iter()
            .any(|alias| alias.pattern == "/v1/query"));

        let prometheus = catalog
            .routes()
            .find(|route| route.id == "prometheus.query.get")
            .expect("prometheus query route is discovered");
        assert!(prometheus
            .aliases
            .iter()
            .any(|alias| alias.pattern == "/prometheus/api/v1/query"));

        let physical = catalog
            .routes()
            .find(|route| route.id == "physical.metadata")
            .expect("physical metadata route is discovered");
        assert!(physical
            .aliases
            .iter()
            .any(|alias| alias.pattern == "/v1/ops/physical/metadata"));

        let graph_job = catalog
            .routes()
            .find(|route| route.id == "graph.jobs.queue")
            .expect("graph job queue route is discovered");
        assert!(graph_job
            .aliases
            .iter()
            .any(|alias| alias.pattern == "/v1/graph/jobs/queue"));
    }

    #[test]
    fn stable_product_routes_have_v1_canonical_entry() {
        let catalog = build_discovered_route_catalog().unwrap();
        let missing: Vec<&str> = catalog
            .routes()
            .filter(|route| route.stability == RouteStability::Stable)
            .filter(|route| route.audience != RouteAudience::CompatibilityAdapter)
            .filter(|route| route.family != "health")
            .filter(|route| {
                !matches!(
                    route.pattern,
                    "/health"
                        | "/ready"
                        | "/ready/query"
                        | "/ready/write"
                        | "/ready/repair"
                        | "/ready/serverless"
                        | "/ready/serverless/query"
                        | "/ready/serverless/write"
                        | "/ready/serverless/repair"
                        | "/grpc"
                )
            })
            .filter(|route| {
                !route.pattern.starts_with("/v1/")
                    && !route
                        .aliases
                        .iter()
                        .any(|alias| alias.pattern.starts_with("/v1/"))
            })
            .map(|route| route.id)
            .collect();

        assert!(
            missing.is_empty(),
            "stable product routes missing canonical /v1 entry: {missing:?}"
        );
    }

    #[test]
    fn readiness_probes_bypass_quota_but_capabilities_do_not() {
        let catalog = build_discovered_route_catalog().unwrap();
        for path in [
            "/health",
            "/ready",
            "/ready/query",
            "/ready/write",
            "/ready/repair",
            "/ready/serverless",
            "/ready/serverless/query",
            "/ready/serverless/write",
            "/ready/serverless/repair",
        ] {
            let matched = catalog
                .find(RouteMethod::Get, path)
                .unwrap_or_else(|| panic!("missing probe route {path}"));
            assert!(
                matched
                    .spec
                    .middlewares
                    .contains(&RouteMiddleware::QuotaBypass),
                "probe route {path} must bypass quota"
            );
        }

        let capabilities = catalog
            .find(RouteMethod::Get, "/capabilities")
            .expect("missing capabilities route");
        assert!(
            capabilities
                .spec
                .middlewares
                .contains(&RouteMiddleware::QuotaGate),
            "capabilities should remain quota-gated"
        );
    }
}
