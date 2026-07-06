//! Client-side registries (SPEC-00 §3.5, US-0005): router registration
//! patterns → `Screen` nodes, data-fetch call shapes → `FETCHES` edges.
//! Like every registry, this is data, not inference — the language adapter
//! consults it while walking the AST and emits [`FetchSite`] facts; the
//! `events` crate resolves fetch URLs against recovered endpoints.

use crate::events::IdentityExpr;

/// Router registry: JSX route components proven from their module import.
pub struct RouterRegistry {
    /// npm modules whose import marks a file as using this router.
    pub modules: &'static [&'static str],
    /// JSX component whose `path`/`element` attributes declare a screen.
    pub route_component: &'static str,
}

/// React Router (v6+; `react-router` and `react-router-dom` share the API).
pub const REACT_ROUTER: RouterRegistry = RouterRegistry {
    modules: &["react-router-dom", "react-router"],
    route_component: "Route",
};

/// Directory whose file structure declares Next.js pages-router screens
/// (`pages/users/[id].tsx` → route `/users/[id]`).
pub const NEXT_PAGES_DIR: &str = "pages";

/// A data-fetch call site emitted by a language adapter — the client-side
/// analog of an event site. The URL classifies exactly like a channel
/// identity (literal / env ref / computed); the `events` crate resolves it
/// against recovered `Endpoint` nodes (AC-0014).
#[derive(Debug, Clone)]
pub struct FetchSite {
    /// HTTP method, uppercase (`GET` when the call shape defaults it).
    pub method: String,
    /// The URL expression, classified.
    pub url: IdentityExpr,
    /// Symbol id of the enclosing component/function, if any.
    pub symbol: Option<String>,
    /// File the site was found in (repo-relative).
    pub path: String,
    /// Byte span of the matched call.
    pub byte_start: u64,
    /// End of the matched call's byte span.
    pub byte_end: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn react_router_registry_shape() {
        assert!(REACT_ROUTER.modules.contains(&"react-router-dom"));
        assert_eq!(REACT_ROUTER.route_component, "Route");
    }
}
