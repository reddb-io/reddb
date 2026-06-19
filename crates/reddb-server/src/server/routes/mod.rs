use super::route_catalog::{RouteCatalog, RouteCatalogError, RouteRegistry};
use std::sync::OnceLock;

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
    use crate::server::route_catalog::RouteMethod;

    #[test]
    fn build_time_discovery_registers_route_files() {
        let catalog = build_discovered_route_catalog().unwrap();
        let route_ids: Vec<&str> = catalog.routes().map(|route| route.id).collect();

        assert!(route_ids.contains(&"health.live"));
        assert!(route_ids.contains(&"health.ready"));
        assert!(route_ids.contains(&"health.startup"));
    }

    #[test]
    fn discovered_routes_are_matchable() {
        let catalog = build_discovered_route_catalog().unwrap();
        let matched = catalog.find(RouteMethod::Get, "/health/live").unwrap();

        assert_eq!(matched.spec.id, "health.live");
    }
}
