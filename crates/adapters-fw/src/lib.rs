//! Framework registries: versioned deterministic knowledge mapping framework
//! registration patterns to endpoints (SPEC-00 §3.3) and event SDK call
//! shapes to channels (§3.4). Registries are data, not inference — a
//! language adapter consults them while walking the AST.

pub mod events;

/// HTTP framework registry (Express family first; Fastify/Nest at M1+).
///
/// An endpoint registration is `receiver.method(route, handler)` where the
/// receiver is a tracked framework object (e.g. a variable bound to
/// `express()` / `express.Router()`) and `method` is in this registry.
pub struct HttpFrameworkRegistry {
    /// npm module whose import marks a file as using this framework.
    pub module_name: &'static str,
    /// Factory call patterns that create a routable object, matched against
    /// the callee text of a variable initializer (e.g. `express`,
    /// `express.Router`).
    pub factories: &'static [&'static str],
    methods: &'static [&'static str],
}

/// Express registry (SPEC-00 M1 proving ground).
pub const EXPRESS: HttpFrameworkRegistry = HttpFrameworkRegistry {
    module_name: "express",
    factories: &["express", "express.Router"],
    methods: &[
        "get", "post", "put", "delete", "patch", "options", "head", "all",
    ],
};

impl HttpFrameworkRegistry {
    /// If `prop` is a route-registration method, returns its canonical
    /// uppercase HTTP verb (`all` maps to `ALL`).
    pub fn http_method(&self, prop: &str) -> Option<String> {
        self.methods
            .contains(&prop)
            .then(|| prop.to_ascii_uppercase())
    }

    /// True when `callee` (source text of a call's function part) creates a
    /// routable object for this framework.
    pub fn is_factory(&self, callee: &str) -> bool {
        self.factories.contains(&callee)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn express_methods_map_to_verbs() {
        assert_eq!(EXPRESS.http_method("get").as_deref(), Some("GET"));
        assert_eq!(EXPRESS.http_method("all").as_deref(), Some("ALL"));
        // `listen`, `use` and arbitrary methods are not endpoint registrations.
        assert_eq!(EXPRESS.http_method("listen"), None);
        assert_eq!(EXPRESS.http_method("use"), None);
    }

    #[test]
    fn express_factories() {
        assert!(EXPRESS.is_factory("express"));
        assert!(EXPRESS.is_factory("express.Router"));
        assert!(!EXPRESS.is_factory("fetch"));
    }
}
