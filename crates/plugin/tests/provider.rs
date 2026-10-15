//! `faber:environments/provider`: registration, routing by mount owner, and
//! the seven operations against a small in-memory built-in provider.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use plugin::bind::{self, Binding};
use plugin::environments::{
    Entry, FsError, Kind, MOUNTS, PROVIDER, Provider, ProviderRoutes, SharedProvider,
};
use plugin::registry::RegisterError;
use plugin::{
    ConfigError, Content, Json, Manifest, Message, Notices, Phase, Plugin, Registry, SessionCtx,
    ToolCall, ToolResult,
};
use serde_json::json;

/// Files of every mount, keyed by (session, mount, path). Directories are
/// entries with no data. Versions count writes, so a stale `if-version` is
/// caught.
#[derive(Default)]
struct Memory {
    nodes: Mutex<BTreeMap<(String, String, String), Node>>,
    calls: Mutex<Vec<String>>,
}

#[derive(Clone)]
struct Node {
    data: Option<Vec<u8>>,
    version: u64,
}

fn parent(path: &str) -> &str {
    match path.rfind('/') {
        Some(0) => "/",
        Some(at) => &path[..at],
        None => "/",
    }
}

fn leaf(path: &str) -> String {
    path.rsplit('/').next().unwrap_or_default().to_owned()
}

impl Memory {
    fn key(ctx: &SessionCtx, mount: &str, path: &str) -> (String, String, String) {
        (ctx.session_id().into(), mount.into(), path.into())
    }

    fn entry(path: &str, node: &Node) -> Entry {
        Entry {
            name: leaf(path),
            kind: if node.data.is_some() {
                Kind::File
            } else {
                Kind::Dir
            },
            size: node.data.as_ref().map_or(0, |d| d.len() as u64),
            mtime: 0,
            version: node.version.to_string(),
        }
    }

    fn root() -> Node {
        Node {
            data: None,
            version: 0,
        }
    }

    fn get(&self, ctx: &SessionCtx, mount: &str, path: &str) -> Option<Node> {
        if path == "/" {
            return Some(Memory::root());
        }
        self.nodes
            .lock()
            .unwrap()
            .get(&Memory::key(ctx, mount, path))
            .cloned()
    }

    fn require_dir(&self, ctx: &SessionCtx, mount: &str, path: &str) -> Result<(), FsError> {
        match self.get(ctx, mount, path) {
            None => Err(FsError::NotFound),
            Some(node) if node.data.is_some() => Err(FsError::NotADirectory),
            Some(_) => Ok(()),
        }
    }

    fn children(&self, ctx: &SessionCtx, mount: &str, dir: &str) -> Vec<(String, Node)> {
        self.nodes
            .lock()
            .unwrap()
            .iter()
            .filter(|((session, m, path), _)| {
                session == ctx.session_id() && m == mount && parent(path) == dir && path != "/"
            })
            .map(|((_, _, path), node)| (path.clone(), node.clone()))
            .collect()
    }
}

#[async_trait]
impl Provider for Memory {
    async fn stat(&self, ctx: &SessionCtx, mount: &str, path: &str) -> Result<Entry, FsError> {
        self.calls.lock().unwrap().push(format!(
            "{} {} stat {mount} {path}",
            ctx.binding(),
            ctx.session_id()
        ));
        let node = self.get(ctx, mount, path).ok_or(FsError::NotFound)?;
        Ok(Memory::entry(path, &node))
    }

    async fn read_dir(
        &self,
        ctx: &SessionCtx,
        mount: &str,
        path: &str,
    ) -> Result<Vec<Entry>, FsError> {
        self.require_dir(ctx, mount, path)?;
        Ok(self
            .children(ctx, mount, path)
            .iter()
            .map(|(path, node)| Memory::entry(path, node))
            .collect())
    }

    async fn read(
        &self,
        ctx: &SessionCtx,
        mount: &str,
        path: &str,
        offset: u64,
        max: u32,
    ) -> Result<Vec<u8>, FsError> {
        let node = self.get(ctx, mount, path).ok_or(FsError::NotFound)?;
        let data = node.data.ok_or(FsError::IsADirectory)?;
        let start = (offset as usize).min(data.len());
        let end = (start + max as usize).min(data.len());
        Ok(data[start..end].to_vec())
    }

    async fn put(
        &self,
        ctx: &SessionCtx,
        mount: &str,
        path: &str,
        data: Vec<u8>,
        if_version: Option<String>,
        create_only: bool,
    ) -> Result<Entry, FsError> {
        self.require_dir(ctx, mount, parent(path))?;
        let existing = self.get(ctx, mount, path);
        let version = match &existing {
            Some(node) if node.data.is_none() => return Err(FsError::IsADirectory),
            Some(_) if create_only => return Err(FsError::Exists),
            Some(node) => {
                if if_version.is_some_and(|v| v != node.version.to_string()) {
                    return Err(FsError::Conflict);
                }
                node.version + 1
            }
            None if if_version.is_some() => return Err(FsError::Conflict),
            None => 1,
        };
        let node = Node {
            data: Some(data),
            version,
        };
        self.nodes
            .lock()
            .unwrap()
            .insert(Memory::key(ctx, mount, path), node.clone());
        Ok(Memory::entry(path, &node))
    }

    async fn mkdir(&self, ctx: &SessionCtx, mount: &str, path: &str) -> Result<Entry, FsError> {
        self.require_dir(ctx, mount, parent(path))?;
        if self.get(ctx, mount, path).is_some() {
            return Err(FsError::Exists);
        }
        let node = Node {
            data: None,
            version: 1,
        };
        self.nodes
            .lock()
            .unwrap()
            .insert(Memory::key(ctx, mount, path), node.clone());
        Ok(Memory::entry(path, &node))
    }

    async fn rename(
        &self,
        ctx: &SessionCtx,
        mount: &str,
        src: &str,
        dst: &str,
    ) -> Result<(), FsError> {
        let node = self.get(ctx, mount, src).ok_or(FsError::NotFound)?;
        if node.data.is_none() {
            // Directories are not moved by this test provider.
            return Err(FsError::Denied);
        }
        self.require_dir(ctx, mount, parent(dst))?;
        let mut nodes = self.nodes.lock().unwrap();
        nodes.remove(&Memory::key(ctx, mount, src));
        nodes.insert(Memory::key(ctx, mount, dst), node);
        Ok(())
    }

    async fn remove(
        &self,
        ctx: &SessionCtx,
        mount: &str,
        path: &str,
        recursive: bool,
    ) -> Result<(), FsError> {
        let node = self.get(ctx, mount, path).ok_or(FsError::NotFound)?;
        let children = self.children(ctx, mount, path);
        if node.data.is_none() && !children.is_empty() && !recursive {
            return Err(FsError::NotEmpty);
        }
        let prefix = format!("{path}/");
        self.nodes.lock().unwrap().retain(|(session, m, p), _| {
            !(session == ctx.session_id() && m == mount && (p == path || p.starts_with(&prefix)))
        });
        Ok(())
    }
}

/// A built-in that mounts: imports `mounts`, exports `provider`.
struct Mounter {
    manifest: Manifest,
    provider: Option<SharedProvider>,
}

fn manifest(id: &str) -> Manifest {
    serde_json::from_value(json!({
        "id": id,
        "name": id,
        "version": "1.0.0",
        "description": "",
        "tools": [],
        "config": { "version": 1, "schema": { "type": "object" }, "default": {} }
    }))
    .unwrap()
}

fn mounter(id: &str, provider: Option<SharedProvider>) -> Mounter {
    Mounter {
        manifest: manifest(id),
        provider,
    }
}

#[async_trait]
impl Plugin for Mounter {
    fn manifest(&self) -> &Manifest {
        &self.manifest
    }
    fn exports(&self) -> Vec<String> {
        vec![PROVIDER.to_owned()]
    }
    fn provider(&self) -> Option<SharedProvider> {
        self.provider.clone()
    }
    async fn open(&self, _: &SessionCtx, _: Phase, _: &Json) -> Result<Vec<Content>, String> {
        Ok(Vec::new())
    }
    async fn handle_tool(&self, _: &SessionCtx, _: &Json, _: ToolCall) -> ToolResult {
        ToolResult::ok("")
    }
    fn validate(&self, config: Json) -> Result<Json, Vec<ConfigError>> {
        Ok(config)
    }
    fn migrate(&self, config: Json, _: u32) -> Result<Json, Vec<ConfigError>> {
        Ok(config)
    }
    fn on_changed(&self, _: &Json, _: &Json) -> Vec<Message> {
        Vec::new()
    }
    fn on_removed(&self, _: &Json) -> Vec<Message> {
        Vec::new()
    }
}

fn binding(plugin: &str, order: i64) -> Binding {
    Binding {
        plugin: plugin.into(),
        version: "1.0.0".into(),
        config_version: 1,
        config: json!({}),
        enabled: true,
        order,
    }
}

#[test]
fn naming_the_export_without_a_provider_is_refused() {
    let mut registry = Registry::new();
    let refused = registry
        .register(Arc::new(mounter("hollow", None)), false)
        .unwrap_err();
    assert!(matches!(refused, RegisterError::ProviderMissing(id) if id == "hollow"));
}

#[test]
fn many_bindings_may_export_provider_in_one_project() {
    let memory: SharedProvider = Arc::new(Memory::default());
    let mut registry = Registry::new();
    for id in ["alpha", "beta"] {
        registry
            .register(Arc::new(mounter(id, Some(Arc::clone(&memory)))), false)
            .unwrap();
    }
    let bindings = [binding("alpha", 1), binding("beta", 2)];
    bind::check_graph(&registry, &bindings).unwrap();
    // Nothing imports a routed interface, so neither keeps the other bound.
    assert!(bind::importers(&registry, &bindings, "alpha").is_empty());
    assert!(
        !bind::providers(&registry, &bindings)
            .unwrap()
            .contains_key(PROVIDER)
    );
    assert!(
        !bind::providers(&registry, &bindings)
            .unwrap()
            .contains_key(MOUNTS)
    );
}

#[tokio::test]
async fn calls_route_to_the_owning_binding_in_its_own_name() {
    let memory = Arc::new(Memory::default());
    let mut registry = Registry::new();
    let shared: SharedProvider = memory.clone();
    registry
        .register(Arc::new(mounter("alpha", Some(Arc::clone(&shared)))), false)
        .unwrap();
    registry
        .register(Arc::new(mounter("beta", Some(shared))), false)
        .unwrap();
    let mut off = binding("beta", 2);
    off.enabled = false;
    let routes = ProviderRoutes::new(&registry, &[binding("alpha", 1), off]);
    assert!(routes.serves("alpha"));
    assert!(
        !routes.serves("beta"),
        "a turned-off binding serves nothing"
    );
    assert!(!routes.serves("gamma"));
    assert_eq!(routes.plugins().collect::<Vec<_>>(), ["alpha"]);

    let notices = Notices::new();
    let (provider, ctx) = routes.route("alpha", "s1", &notices).unwrap();
    provider.stat(&ctx, "docs", "/").await.unwrap();
    assert_eq!(*memory.calls.lock().unwrap(), ["alpha s1 stat docs /"]);
}

#[tokio::test]
async fn the_seven_operations_cover_read_patch_and_listing() {
    let memory = Memory::default();
    let ctx = SessionCtx::new("s1", "alpha", 1, Notices::new());
    let other = SessionCtx::new("s2", "alpha", 1, Notices::new());

    // patch add under a missing directory: mkdir first.
    assert_eq!(
        memory
            .put(&ctx, "m", "/src/a.txt", b"one".to_vec(), None, true)
            .await,
        Err(FsError::NotFound)
    );
    let dir = memory.mkdir(&ctx, "m", "/src").await.unwrap();
    assert_eq!(dir.kind, Kind::Dir);
    let added = memory
        .put(&ctx, "m", "/src/a.txt", b"one".to_vec(), None, true)
        .await
        .unwrap();
    assert_eq!((added.size, added.kind), (3, Kind::File));

    // create-only refuses an existing file.
    assert_eq!(
        memory
            .put(&ctx, "m", "/src/a.txt", b"x".to_vec(), None, true)
            .await,
        Err(FsError::Exists)
    );

    // read a window.
    let stat = memory.stat(&ctx, "m", "/src/a.txt").await.unwrap();
    assert_eq!(
        memory.read(&ctx, "m", "/src/a.txt", 1, 10).await.unwrap(),
        b"ne"
    );

    // update with if-version; a stale one conflicts.
    memory
        .put(
            &ctx,
            "m",
            "/src/a.txt",
            b"two".to_vec(),
            Some(stat.version.clone()),
            false,
        )
        .await
        .unwrap();
    assert_eq!(
        memory
            .put(
                &ctx,
                "m",
                "/src/a.txt",
                b"three".to_vec(),
                Some(stat.version),
                false
            )
            .await,
        Err(FsError::Conflict)
    );

    // listing carries kinds; sessions never see each other's content.
    let listed = memory.read_dir(&ctx, "m", "/src").await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(
        (listed[0].name.as_str(), listed[0].kind),
        ("a.txt", Kind::File)
    );
    assert_eq!(
        memory.stat(&other, "m", "/src/a.txt").await,
        Err(FsError::NotFound)
    );

    // move, then delete: a non-empty directory needs recursive.
    memory
        .rename(&ctx, "m", "/src/a.txt", "/src/b.txt")
        .await
        .unwrap();
    assert_eq!(
        memory.remove(&ctx, "m", "/src", false).await,
        Err(FsError::NotEmpty)
    );
    memory.remove(&ctx, "m", "/src", true).await.unwrap();
    assert_eq!(
        memory.stat(&ctx, "m", "/src/b.txt").await,
        Err(FsError::NotFound)
    );
}
