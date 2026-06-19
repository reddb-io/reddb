use crate::server::route_catalog::{
    ListenerSurface, RouteAlias, RouteAudience, RouteAuth, RouteEntry, RouteGroupDefaults,
    RouteMethod, RouteRegistry, RouteStability,
};
use crate::server::routes::common::STANDARD_MIDDLEWARE;

const REPO_SURFACES: &[ListenerSurface] = &[ListenerSurface::Public];

const REPO_USER: RouteGroupDefaults = RouteGroupDefaults {
    family: "repo",
    audience: RouteAudience::Client,
    auth: RouteAuth::UserRequired,
    surfaces: REPO_SURFACES,
    stability: RouteStability::Stable,
    middlewares: STANDARD_MIDDLEWARE,
};

const REPO_ALIASES: &[RouteAlias] = &[RouteAlias::canonical(
    RouteMethod::Get,
    "/v1/repo",
    "canonical v1 repo path",
)];

const REPO_ROUTES: &[RouteEntry] = &[
    RouteEntry::with_aliases("repo.info", RouteMethod::Get, "/repo", REPO_ALIASES),
    RouteEntry::new("repo.refs.list", RouteMethod::Get, "/repo/refs"),
    RouteEntry::new("repo.refs.heads.list", RouteMethod::Get, "/repo/refs/heads"),
    RouteEntry::new("repo.refs.heads.create", RouteMethod::Post, "/repo/refs/heads"),
    RouteEntry::new("repo.refs.heads.show", RouteMethod::Get, "/repo/refs/heads/*"),
    RouteEntry::new("repo.refs.heads.move", RouteMethod::Put, "/repo/refs/heads/*"),
    RouteEntry::new("repo.refs.heads.delete", RouteMethod::Delete, "/repo/refs/heads/*"),
    RouteEntry::new("repo.refs.tags.list", RouteMethod::Get, "/repo/refs/tags"),
    RouteEntry::new("repo.refs.tags.create", RouteMethod::Post, "/repo/refs/tags"),
    RouteEntry::new("repo.refs.tags.show", RouteMethod::Get, "/repo/refs/tags/*"),
    RouteEntry::new("repo.refs.tags.delete", RouteMethod::Delete, "/repo/refs/tags/*"),
    RouteEntry::new("repo.commits.list", RouteMethod::Get, "/repo/commits"),
    RouteEntry::new("repo.commits.create", RouteMethod::Post, "/repo/commits"),
    RouteEntry::new("repo.commits.show", RouteMethod::Get, "/repo/commits/:hash"),
    RouteEntry::new("repo.commits.diff", RouteMethod::Get, "/repo/commits/:a/diff/:b"),
    RouteEntry::new("repo.commits.lca", RouteMethod::Get, "/repo/commits/:a/lca/:b"),
    RouteEntry::new("repo.sessions.status", RouteMethod::Get, "/repo/sessions/:conn"),
    RouteEntry::new(
        "repo.sessions.checkout",
        RouteMethod::Post,
        "/repo/sessions/:conn/checkout",
    ),
    RouteEntry::new("repo.sessions.merge", RouteMethod::Post, "/repo/sessions/:conn/merge"),
    RouteEntry::new("repo.sessions.reset", RouteMethod::Post, "/repo/sessions/:conn/reset"),
    RouteEntry::new(
        "repo.sessions.cherry_pick",
        RouteMethod::Post,
        "/repo/sessions/:conn/cherry-pick",
    ),
    RouteEntry::new("repo.sessions.revert", RouteMethod::Post, "/repo/sessions/:conn/revert"),
    RouteEntry::new("repo.merges.show", RouteMethod::Get, "/repo/merges/:msid"),
    RouteEntry::new(
        "repo.merges.conflicts",
        RouteMethod::Get,
        "/repo/merges/:msid/conflicts",
    ),
    RouteEntry::new(
        "repo.merges.conflicts.resolve",
        RouteMethod::Post,
        "/repo/merges/:msid/conflicts/:cid/resolve",
    ),
    RouteEntry::new("repo.collections.vcs.show", RouteMethod::Get, "/collections/:name/vcs"),
    RouteEntry::new("repo.collections.vcs.set", RouteMethod::Put, "/collections/:name/vcs"),
];

pub(crate) fn register(registry: &mut RouteRegistry) {
    registry.routes(REPO_USER, REPO_ROUTES);
}
