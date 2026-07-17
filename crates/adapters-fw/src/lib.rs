//! Framework registries: versioned deterministic knowledge mapping framework
//! registration patterns to endpoints (SPEC-00 §3.3) and event SDK call
//! shapes to channels (§3.4). Registries are data, not inference — a
//! language adapter consults them while walking the AST.

pub mod client;
pub mod events;
pub mod tsconfig;

/// HTTP framework registry for factory-created routers (Express/Fastify).
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

/// Fastify registry. Its route-registration shape is the same deterministic
/// `receiver.method(path, handler)` form as Express, but the receiver must be
/// proven to come from the `fastify` factory.
pub const FASTIFY: HttpFrameworkRegistry = HttpFrameworkRegistry {
    module_name: "fastify",
    factories: &["fastify"],
    methods: &[
        "get", "post", "put", "delete", "patch", "options", "head", "all",
    ],
};

/// Factory-created HTTP framework registries consulted by language adapters.
pub const HTTP_FACTORIES: &[&HttpFrameworkRegistry] = &[&EXPRESS, &FASTIFY];

/// NestJS's decorator registry. These names are only authoritative when the
/// decorator binding is import-proven from `@nestjs/common`.
pub struct NestRegistry {
    /// npm module that owns the decorators.
    pub module_name: &'static str,
    /// Class decorator that supplies the route prefix.
    pub controller: &'static str,
    methods: &'static [(&'static str, &'static str)],
}

/// NestJS controller and HTTP method decorators.
pub const NEST: NestRegistry = NestRegistry {
    module_name: "@nestjs/common",
    controller: "Controller",
    methods: &[
        ("Get", "GET"),
        ("Post", "POST"),
        ("Put", "PUT"),
        ("Delete", "DELETE"),
        ("Patch", "PATCH"),
        ("Options", "OPTIONS"),
        ("Head", "HEAD"),
        ("All", "ALL"),
    ],
};

impl NestRegistry {
    /// Map an import-proven Nest decorator name to its canonical HTTP verb.
    pub fn http_method(&self, decorator: &str) -> Option<&'static str> {
        self.methods
            .iter()
            .find_map(|(name, verb)| (*name == decorator).then_some(*verb))
    }
}

impl HttpFrameworkRegistry {
    /// If `prop` is a route-registration method, returns its canonical
    /// uppercase HTTP verb (`all` maps to `ALL`).
    pub fn http_method(&self, prop: &str) -> Option<String> {
        self.methods
            .contains(&prop)
            .then(|| prop.to_ascii_uppercase())
    }

    /// True when `callee` has a registered factory shape. The language adapter
    /// must separately prove the base identifier is imported from
    /// [`Self::module_name`], so default-import aliases remain deterministic.
    pub fn is_factory(&self, callee: &str) -> bool {
        match callee.split_once('.') {
            None => self.factories.iter().any(|factory| !factory.contains('.')),
            Some((_, member)) => self.factories.iter().any(|factory| {
                factory
                    .split_once('.')
                    .is_some_and(|(_, registered)| registered == member)
            }),
        }
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
        assert!(EXPRESS.is_factory("makeExpress"));
        assert!(EXPRESS.is_factory("e.Router"));
        assert!(!EXPRESS.is_factory("express.createApplication"));
    }

    #[test]
    fn fastify_and_nest_registry_shapes() {
        assert!(FASTIFY.is_factory("fastify"));
        assert!(FASTIFY.is_factory("Fastify"));
        assert!(!FASTIFY.is_factory("fastify.Router"));
        assert_eq!(NEST.http_method("Get"), Some("GET"));
        assert_eq!(NEST.http_method("SubscribeMessage"), None);
    }
}
