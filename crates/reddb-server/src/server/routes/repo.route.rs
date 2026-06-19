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

macro_rules! repo_aliases {
    ($method:expr, $pattern:expr) => {
        &[RouteAlias::canonical(
            $method,
            $pattern,
            "canonical v1 repo path",
        )]
    };
}

const REPO_ROUTES: &[RouteEntry] = &[
    RouteEntry::with_aliases("repo.info", RouteMethod::Get, "/repo", REPO_ALIASES),
    RouteEntry::with_aliases(
        "repo.refs.list",
        RouteMethod::Get,
        "/repo/refs",
        repo_aliases!(RouteMethod::Get, "/v1/repo/refs"),
    ),
    RouteEntry::with_aliases(
        "repo.refs.heads.list",
        RouteMethod::Get,
        "/repo/refs/heads",
        repo_aliases!(RouteMethod::Get, "/v1/repo/refs/heads"),
    ),
    RouteEntry::with_aliases(
        "repo.refs.heads.create",
        RouteMethod::Post,
        "/repo/refs/heads",
        repo_aliases!(RouteMethod::Post, "/v1/repo/refs/heads"),
    ),
    RouteEntry::with_aliases(
        "repo.refs.heads.show",
        RouteMethod::Get,
        "/repo/refs/heads/*",
        repo_aliases!(RouteMethod::Get, "/v1/repo/refs/heads/*"),
    ),
    RouteEntry::with_aliases(
        "repo.refs.heads.move",
        RouteMethod::Put,
        "/repo/refs/heads/*",
        repo_aliases!(RouteMethod::Put, "/v1/repo/refs/heads/*"),
    ),
    RouteEntry::with_aliases(
        "repo.refs.heads.delete",
        RouteMethod::Delete,
        "/repo/refs/heads/*",
        repo_aliases!(RouteMethod::Delete, "/v1/repo/refs/heads/*"),
    ),
    RouteEntry::with_aliases(
        "repo.refs.tags.list",
        RouteMethod::Get,
        "/repo/refs/tags",
        repo_aliases!(RouteMethod::Get, "/v1/repo/refs/tags"),
    ),
    RouteEntry::with_aliases(
        "repo.refs.tags.create",
        RouteMethod::Post,
        "/repo/refs/tags",
        repo_aliases!(RouteMethod::Post, "/v1/repo/refs/tags"),
    ),
    RouteEntry::with_aliases(
        "repo.refs.tags.show",
        RouteMethod::Get,
        "/repo/refs/tags/*",
        repo_aliases!(RouteMethod::Get, "/v1/repo/refs/tags/*"),
    ),
    RouteEntry::with_aliases(
        "repo.refs.tags.delete",
        RouteMethod::Delete,
        "/repo/refs/tags/*",
        repo_aliases!(RouteMethod::Delete, "/v1/repo/refs/tags/*"),
    ),
    RouteEntry::with_aliases(
        "repo.commits.list",
        RouteMethod::Get,
        "/repo/commits",
        repo_aliases!(RouteMethod::Get, "/v1/repo/commits"),
    ),
    RouteEntry::with_aliases(
        "repo.commits.create",
        RouteMethod::Post,
        "/repo/commits",
        repo_aliases!(RouteMethod::Post, "/v1/repo/commits"),
    ),
    RouteEntry::with_aliases(
        "repo.commits.show",
        RouteMethod::Get,
        "/repo/commits/:hash",
        repo_aliases!(RouteMethod::Get, "/v1/repo/commits/:hash"),
    ),
    RouteEntry::with_aliases(
        "repo.commits.diff",
        RouteMethod::Get,
        "/repo/commits/:a/diff/:b",
        repo_aliases!(RouteMethod::Get, "/v1/repo/commits/:a/diff/:b"),
    ),
    RouteEntry::with_aliases(
        "repo.commits.lca",
        RouteMethod::Get,
        "/repo/commits/:a/lca/:b",
        repo_aliases!(RouteMethod::Get, "/v1/repo/commits/:a/lca/:b"),
    ),
    RouteEntry::with_aliases(
        "repo.sessions.status",
        RouteMethod::Get,
        "/repo/sessions/:conn",
        repo_aliases!(RouteMethod::Get, "/v1/repo/sessions/:conn"),
    ),
    RouteEntry::with_aliases(
        "repo.sessions.checkout",
        RouteMethod::Post,
        "/repo/sessions/:conn/checkout",
        repo_aliases!(RouteMethod::Post, "/v1/repo/sessions/:conn/checkout"),
    ),
    RouteEntry::with_aliases(
        "repo.sessions.merge",
        RouteMethod::Post,
        "/repo/sessions/:conn/merge",
        repo_aliases!(RouteMethod::Post, "/v1/repo/sessions/:conn/merge"),
    ),
    RouteEntry::with_aliases(
        "repo.sessions.reset",
        RouteMethod::Post,
        "/repo/sessions/:conn/reset",
        repo_aliases!(RouteMethod::Post, "/v1/repo/sessions/:conn/reset"),
    ),
    RouteEntry::with_aliases(
        "repo.sessions.cherry_pick",
        RouteMethod::Post,
        "/repo/sessions/:conn/cherry-pick",
        repo_aliases!(RouteMethod::Post, "/v1/repo/sessions/:conn/cherry-pick"),
    ),
    RouteEntry::with_aliases(
        "repo.sessions.revert",
        RouteMethod::Post,
        "/repo/sessions/:conn/revert",
        repo_aliases!(RouteMethod::Post, "/v1/repo/sessions/:conn/revert"),
    ),
    RouteEntry::with_aliases(
        "repo.merges.show",
        RouteMethod::Get,
        "/repo/merges/:msid",
        repo_aliases!(RouteMethod::Get, "/v1/repo/merges/:msid"),
    ),
    RouteEntry::with_aliases(
        "repo.merges.conflicts",
        RouteMethod::Get,
        "/repo/merges/:msid/conflicts",
        repo_aliases!(RouteMethod::Get, "/v1/repo/merges/:msid/conflicts"),
    ),
    RouteEntry::with_aliases(
        "repo.merges.conflicts.resolve",
        RouteMethod::Post,
        "/repo/merges/:msid/conflicts/:cid/resolve",
        repo_aliases!(
            RouteMethod::Post,
            "/v1/repo/merges/:msid/conflicts/:cid/resolve"
        ),
    ),
    RouteEntry::with_aliases(
        "repo.collections.vcs.show",
        RouteMethod::Get,
        "/collections/:name/vcs",
        repo_aliases!(RouteMethod::Get, "/v1/repo/collections/:name/vcs"),
    ),
    RouteEntry::with_aliases(
        "repo.collections.vcs.set",
        RouteMethod::Put,
        "/collections/:name/vcs",
        repo_aliases!(RouteMethod::Put, "/v1/repo/collections/:name/vcs"),
    ),
];

pub(crate) fn register(registry: &mut RouteRegistry) {
    registry.routes(REPO_USER, REPO_ROUTES);
}
