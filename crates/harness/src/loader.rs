//! A module loader that resolves a harness's whole module graph and refuses
//! everything else.
//!
//! `abstract.md` §7: "Import control *is* the sandbox." A harness may reach
//! its own module, the capability-object builder Core injects, and the
//! `https:` dependencies that survived the hardcoded allowlist below — nothing
//! else. No filesystem (beyond a directory harness's own tree, resolved at
//! construction), no arbitrary network hosts, no other harness's code.
//!
//! Resolution is Deno's own: the graph is built once with
//! [`deno_graph`]'s `GraphBuilder` against the [`SourceLoader`] here, so
//! relative imports, extensionless imports, `./x` -> `./x/index.ts` fallback,
//! and `https:` imports all behave the way they do in Deno. The isolate then
//! executes from that built graph, with TypeScript transpiled to JavaScript on
//! the way out.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use deno_ast::MediaType;
use deno_core::error::ModuleLoaderError;
use deno_core::{
    ModuleLoadOptions, ModuleLoadReferrer, ModuleLoadResponse, ModuleLoader, ModuleResolveResponse,
    ModuleSource, ModuleSourceCode, ModuleSpecifier, ModuleType, ResolutionKind,
};
use deno_error::JsErrorBox;
use deno_graph::source::{LoadError, LoadFuture, LoadOptions, LoadResponse, Loader};
use deno_graph::{BuildOptions, GraphKind, ModuleGraph};

use crate::graph::Harness;

// A real URL, not a bare scheme: `harness:main` cannot be a base for
// relative-import resolution, and the harness entry is what relative imports
// resolve against. `harness:///main.js` parses with a `/main.js` path and an
// empty host, which makes `harness:///sub.js` a valid relative target.
pub const HARNESS_SPECIFIER: &str = "harness:///main.js";
// Deliberately not an `ext:` specifier: deno_core only allows `ext:` modules
// to be imported from other `ext:`/`node:` modules, and the bootstrap module
// (`harness:bootstrap`) is neither.
pub const CONTEXT_SPECIFIER: &str = "faber:context.js";
/// Not resolved through this loader — the bootstrap module is handed to
/// `load_main_es_module_from_code` directly by `runtime.rs`, since its
/// source is generated fresh per run (it embeds the JSON-encoded input).
pub const BOOTSTRAP_SPECIFIER: &str = "harness:bootstrap";

const CONTEXT_SOURCE: &str = include_str!("context.js");

/// The hardcoded `https:` allowlist. A harness may import from exactly these
/// hosts — everything else is refused before any network request is made.
///
/// This is the trust boundary that replaces the old "no network at all"
/// guarantee. It is deliberately narrow and deliberate: a host is added only
/// when a bundled harness actually needs it, and removing it removes the
/// capability.
const HTTPS_ALLOWLIST: &[&str] = &[
    // Deno's own std. A trusted, reviewed dependency surface.
    "deno.land",
    // jsr.io's CDN serves raw module files behind jsr redirects. We do not
    // resolve `jsr:` specifiers (out of scope); an https import that lands
    // here is allowed to fetch the bytes, no more.
    "jsr.io",
];

/// Whether a host is on the hardcoded allowlist. Exact host match — an
/// allowlist entry of `deno.land` does not admit `evil.deno.land`.
fn host_allowed(host: &str) -> bool {
    HTTPS_ALLOWLIST.contains(&host)
}

/// The loader that serves a harness's module graph to the isolate, resolving
/// relative and `https:` imports through `deno_graph` and transpiling
/// TypeScript to JavaScript.
pub struct HarnessLoader {
    /// The fully-built, resolved graph — every module the harness may reach.
    graph: Rc<ModuleGraph>,
    /// The harness's entry specifier, resolved by name by deno_core (the
    /// bootstrap imports it; no module in the graph depends on it).
    main: ModuleSpecifier,
}

impl HarnessLoader {
    /// Builds the module graph for `harness`, fetching any allowlisted
    /// `https:` dependencies. Refuses anything not in the harness's own graph
    /// and not on the allowlist.
    pub async fn build(harness: &Harness) -> Result<Self, HarnessLoaderError> {
        let loader = SourceLoader::new(harness);
        let root = ModuleSpecifier::parse(&harness.main)
            .map_err(|e| HarnessLoaderError::Specifier(harness.main.clone(), e))?;

        let main = root.clone();
        let mut graph = ModuleGraph::new(GraphKind::CodeOnly);
        graph
            .build(
                vec![root],
                Default::default(),
                &loader,
                BuildOptions::default(),
            )
            .await;

        graph
            .valid()
            .map_err(|e| HarnessLoaderError::Graph(Box::new(e)))?;
        Ok(Self {
            graph: Rc::new(graph),
            main,
        })
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HarnessLoaderError {
    #[error("harness entry specifier `{0}` is not a valid URL: {1}")]
    Specifier(String, url::ParseError),
    #[error("harness module graph could not be built: {0}")]
    Graph(#[from] Box<deno_graph::ModuleGraphError>),
}

impl ModuleLoader for HarnessLoader {
    fn resolve(
        &self,
        specifier: &str,
        referrer: &str,
        _kind: ResolutionKind,
    ) -> ModuleResolveResponse {
        // The harness entry, the context, and the bootstrap are all root
        // specifiers that deno_core resolves directly (the bootstrap is even
        // handed to `load_main_es_module_from_code` but still resolved first).
        // No module in the graph depends on them, so resolve them by name
        // before consulting the graph.
        if matches!(
            specifier,
            HARNESS_SPECIFIER | CONTEXT_SPECIFIER | BOOTSTRAP_SPECIFIER
        ) {
            return ModuleSpecifier::parse(specifier).map_err(|error| {
                ModuleLoaderError::from(JsErrorBox::generic(format!(
                    "harness module specifier `{specifier}` is not a valid URL: {error}"
                )))
            });
        }
        if specifier == self.main.as_str() {
            return Ok(self.main.clone());
        }

        // Resolve against the built graph — Deno's own resolution rules. The
        // referrer deno_core hands us for the main module's own imports can be
        // the relative marker `.` rather than the main module's URL; in that
        // case the entry module is the base to resolve against.
        let referrer_url = ModuleSpecifier::parse(referrer).ok();
        let entry_url = ModuleSpecifier::parse(HARNESS_SPECIFIER).ok();
        let base: &ModuleSpecifier = match &referrer_url {
            Some(u) => u,
            None => match &entry_url {
                Some(e) => e,
                None => {
                    return Err(ModuleLoaderError::from(JsErrorBox::generic(format!(
                        "harness referrer `{referrer}` is not a valid URL"
                    ))));
                }
            },
        };

        let resolved = self
            .graph
            .resolve_dependency(specifier, base, false)
            .cloned();

        match resolved {
            Some(url) => Ok(url),
            None => Err(ModuleLoaderError::from(JsErrorBox::generic(format!(
                "import of `{specifier}` from `{referrer}` refused: not part of the harness's \
                 module graph or the https allowlist (abstract.md §7)"
            )))),
        }
    }

    fn load(
        &self,
        module_specifier: &ModuleSpecifier,
        _maybe_referrer: Option<&ModuleLoadReferrer>,
        _options: ModuleLoadOptions,
    ) -> ModuleLoadResponse {
        let specifier = module_specifier.as_str();

        // The context module is Core's, served from the embedded constant
        // directly — it is not part of the harness's graph and never fetched.
        if specifier == CONTEXT_SPECIFIER {
            return ModuleLoadResponse::Sync(Ok(ModuleSource::new(
                ModuleType::JavaScript,
                ModuleSourceCode::String(CONTEXT_SOURCE.to_string().into()),
                module_specifier,
                None,
            )));
        }

        let module = self
            .graph
            .modules()
            .find(|m| m.specifier().as_str() == specifier);
        let module = match module {
            Some(m) => m,
            None => {
                return ModuleLoadResponse::Sync(Err(ModuleLoaderError::from(
                    JsErrorBox::generic(format!(
                        "refused to load `{specifier}`: not in the harness module graph"
                    )),
                )));
            }
        };

        // The context module is Core's, served from the embedded constant; it
        // is never the harness's to reach through deno_graph.
        let (source, media_type) = {
            let Some(src) = module.source() else {
                return ModuleLoadResponse::Sync(Err(ModuleLoaderError::from(
                    JsErrorBox::generic(format!("module `{specifier}` carries no source")),
                )));
            };
            (src.to_string(), module.media_type())
        };

        let js = match transpile_if_typescript(source, media_type) {
            Ok(js) => js,
            Err(e) => {
                return ModuleLoadResponse::Sync(Err(ModuleLoaderError::from(
                    JsErrorBox::generic(e),
                )));
            }
        };

        ModuleLoadResponse::Sync(Ok(ModuleSource::new(
            ModuleType::JavaScript,
            ModuleSourceCode::String(js.into()),
            module_specifier,
            None,
        )))
    }
}

/// Transpiles TypeScript to JavaScript, passing JavaScript through unchanged.
fn transpile_if_typescript(source: String, media_type: MediaType) -> Result<String, String> {
    use deno_ast::EmitOptions;
    if !matches!(
        media_type,
        MediaType::TypeScript | MediaType::Mts | MediaType::Cts | MediaType::Tsx
    ) {
        return Ok(source);
    }
    let parsed = deno_ast::parse_program(deno_ast::ParseParams {
        specifier: deno_ast::ModuleSpecifier::parse("harness:///module.ts").unwrap(),
        media_type,
        text: source.into(),
        capture_tokens: false,
        scope_analysis: false,
        maybe_syntax: None,
    })
    .map_err(|e| format!("failed to parse TypeScript: {e}"))?;
    let transpiled = parsed
        .transpile(
            &Default::default(),
            &Default::default(),
            &EmitOptions::default(),
        )
        .map_err(|e| format!("failed to transpile TypeScript: {e}"))?;
    Ok(transpiled.into_source().text)
}

/// The `deno_graph::Loader` that feeds source into the graph builder. The
/// harness's own modules are served from memory; allowlisted `https:` hosts
/// are fetched; every other specifier is refused.
struct SourceLoader<'a> {
    harness: &'a Harness,
    /// Fetched https sources, cached so each URL is fetched once per build.
    fetched: std::cell::RefCell<HashMap<String, String>>,
}

impl<'a> SourceLoader<'a> {
    fn new(harness: &'a Harness) -> Self {
        Self {
            harness,
            fetched: std::cell::RefCell::new(HashMap::new()),
        }
    }
}

impl Loader for SourceLoader<'_> {
    fn load(&self, specifier: &ModuleSpecifier, _options: LoadOptions) -> LoadFuture {
        let spec = specifier.as_str().to_string();

        // Serve the harness's own inline modules from memory.
        if let Some(source) = self.harness.source_for(&spec) {
            let owned = source.to_owned();
            return Box::pin(async move {
                Ok(Some(LoadResponse::Module {
                    specifier: ModuleSpecifier::parse(&spec).unwrap(),
                    content: Arc::from(owned.into_bytes()),
                    maybe_headers: None,
                    mtime: None,
                }))
            });
        }

        // An https specifier: check the allowlist, then fetch (cached).
        if spec.starts_with("https://") {
            let host = ModuleSpecifier::parse(&spec)
                .ok()
                .and_then(|u| u.host_str().map(|h| h.to_string()))
                .unwrap_or_default();
            if !host_allowed(&host) {
                return Box::pin(async move {
                    Err(LoadError::Other(Arc::new(JsErrorBox::generic(format!(
                        "refused to fetch `{spec}`: host `{host}` is not on the harness https allowlist (abstract.md §7)"
                    )))))
                });
            }
            if let Some(cached) = self.fetched.borrow().get(&spec).cloned() {
                return Box::pin(async move {
                    Ok(Some(LoadResponse::Module {
                        specifier: ModuleSpecifier::parse(&spec).unwrap(),
                        content: Arc::from(cached.into_bytes()),
                        maybe_headers: None,
                        mtime: None,
                    }))
                });
            }
            return fetch_module(spec);
        }

        Box::pin(async move {
            Err(LoadError::Other(Arc::new(JsErrorBox::generic(format!(
                "refused to load `{spec}`: not part of the harness graph and not a fetchable https module (abstract.md §7)"
            )))))
        })
    }
}

/// Fetches an allowlisted https module (host already cleared by the caller).
fn fetch_module(spec: String) -> LoadFuture {
    Box::pin(async move {
        let client = reqwest::Client::new();
        let resp = client.get(&spec).send().await.map_err(|e| {
            LoadError::Other(Arc::new(JsErrorBox::generic(format!(
                "failed to fetch `{spec}`: {e}"
            ))))
        })?;
        let bytes = resp.bytes().await.map_err(|e| {
            LoadError::Other(Arc::new(JsErrorBox::generic(format!(
                "failed to read response for `{spec}`: {e}"
            ))))
        })?;
        let content: Arc<[u8]> = Arc::from(bytes.to_vec());
        Ok(Some(LoadResponse::Module {
            specifier: ModuleSpecifier::parse(&spec).unwrap(),
            content,
            maybe_headers: None,
            mtime: None,
        }))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_is_exact_host() {
        assert!(host_allowed("deno.land"));
        assert!(!host_allowed("evil.deno.land"));
        assert!(!host_allowed("npmjs.com"));
        assert!(!host_allowed(""));
    }
}
