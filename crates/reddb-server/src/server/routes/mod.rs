use super::route_catalog::{RouteCatalog, RouteCatalogError, RouteRegistry};

mod generated {
    include!(concat!(env!("OUT_DIR"), "/route_discovery.rs"));
}

pub(crate) fn discovered_route_catalog() -> Result<RouteCatalog, RouteCatalogError> {
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
        let catalog = discovered_route_catalog().unwrap();
        let route_ids: Vec<&str> = catalog.routes().map(|route| route.id).collect();

        assert!(route_ids.contains(&"health.live"));
    }

    #[test]
    fn discovered_routes_are_matchable() {
        let catalog = discovered_route_catalog().unwrap();
        let matched = catalog.find(RouteMethod::Get, "/health/live").unwrap();

        assert_eq!(matched.spec.id, "health.live");
    }
}
