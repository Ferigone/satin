//! Per-document module map. Transport happens on workers; parsed records and
//! execution remain in the owning realm. A cache hit shares module *identity*.
use super::{Consumer, ResourceQueue, import_map::ImportMap};
use crate::{
    loader::{ResourceKind, ResourceRequest},
    network::CredentialsMode,
};
use rust_browser_javascript::{JsError, ModuleId, ModuleRequest, ModuleType, Vm};
use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};
use url::Url;

const MAX_MODULES: usize = 512;
const MAX_MODULE_SOURCE_BYTES: usize = 16 * 1024 * 1024;
const MAX_MODULE_EDGES: usize = 2048;
const MAX_GRAPH_URL_BYTES: usize = 4 * 1024 * 1024;
const MAX_MODULE_URL: usize = 128 * 1024;

#[derive(Clone, Copy)]
pub(super) struct ModuleKey(usize, ModuleType);

pub(super) enum ModuleSource {
    JavaScript(String),
    Typed(Vec<u8>),
}

enum Record {
    Loading,
    Parsed {
        id: ModuleId,
        edges: Vec<(ModuleRequest, ModuleKey)>,
        credentials: CredentialsMode,
    },
    Failed(Arc<str>),
}
pub(super) enum Readiness {
    Pending,
    Ready(ModuleId),
    Failed(Arc<str>),
}

#[derive(Default)]
pub(super) struct ModuleGraph {
    pub(super) imports: ImportMap,
    pub(super) sources: crate::script::inspection_sources::SourceArchive,
    records: Vec<Record>,
    requests: HashMap<(Arc<Url>, ModuleType), ModuleKey>,
    ids: HashMap<ModuleId, ModuleKey>,
    url_bytes: usize,
    source_bytes: usize,
    edges: usize,
}
impl ModuleGraph {
    fn allocate(&mut self, kind: ModuleType) -> Result<ModuleKey, String> {
        if self.records.len() >= MAX_MODULES {
            return Err("Module graph exceeds 512 records".into());
        }
        let key = ModuleKey(self.records.len(), kind);
        self.records.push(Record::Loading);
        Ok(key)
    }

    fn charge_url(&mut self, url: &Url) -> Result<(), String> {
        let len = url.as_str().len();
        if len > MAX_MODULE_URL || self.url_bytes + len > MAX_GRAPH_URL_BYTES {
            return Err("Module graph URL budget exceeded".into());
        }
        self.url_bytes += len;
        Ok(())
    }

    pub(super) fn credentials(&self, id: ModuleId) -> Option<CredentialsMode> {
        let key = self.ids.get(&id)?;
        if let Record::Parsed { credentials, .. } = &self.records[key.0] {
            Some(*credentials)
        } else {
            None
        }
    }

    pub(super) fn resolve_specifier(
        &mut self,
        specifier: &str,
        base: &Arc<Url>,
    ) -> Result<Url, String> {
        // Even an unmapped bare specifier occupies the map's resolution list.
        if specifier.len() > MAX_MODULE_URL
            || self.url_bytes + specifier.len() > MAX_GRAPH_URL_BYTES
        {
            return Err("Module specifier byte budget exceeded".into());
        }
        self.url_bytes += specifier.len();
        self.imports.resolve(specifier, base)
    }

    pub(super) fn external(
        &mut self,
        url: Url,
        client: &Arc<Url>,
        credentials: CredentialsMode,
        integrity: Option<Arc<str>>,
        resources: &mut ResourceQueue,
    ) -> Result<ModuleKey, String> {
        self.external_typed(
            url,
            client,
            credentials,
            integrity,
            resources,
            ModuleType::JavaScript,
        )
    }

    pub(super) fn external_typed(
        &mut self,
        url: Url,
        client: &Arc<Url>,
        credentials: CredentialsMode,
        integrity: Option<Arc<str>>,
        resources: &mut ResourceQueue,
        kind: ModuleType,
    ) -> Result<ModuleKey, String> {
        let url = Arc::new(url);
        if let Some(key) = self.requests.get(&(url.clone(), kind)) {
            return Ok(*key);
        }
        self.charge_url(&url)?;
        let key = self.allocate(kind)?;
        let integrity = integrity.or_else(|| self.imports.integrity(&url));
        let request = ResourceRequest {
            url: url.as_ref().clone(),
            client: client.clone(),
            kind: match kind {
                ModuleType::JavaScript => ResourceKind::ModuleScript,
                ModuleType::Json => ResourceKind::JsonModule,
                ModuleType::Text => ResourceKind::TextModule,
                ModuleType::Bytes => ResourceKind::BytesModule,
            },
            cors: Some(credentials),
            integrity,
            deadline: ResourceQueue::queue_deadline(),
        };
        self.requests.insert((url, kind), key);
        if !matches!(request.url.scheme(), "http" | "https" | "data") {
            self.fail(key, "Module URL is not fetchable in this document".into());
        } else if let Err(error) = resources.queue(request, Consumer::Module(key, credentials)) {
            self.fail(key, error);
        }
        Ok(key)
    }

    pub(super) fn inline(
        &mut self,
        source: String,
        base: Arc<Url>,
        client: &Arc<Url>,
        credentials: CredentialsMode,
        vm: &mut Vm,
        resources: &mut ResourceQueue,
    ) -> Result<ModuleKey, String> {
        let key = self.allocate(ModuleType::JavaScript)?;
        self.parsed_response(
            key,
            ModuleSource::JavaScript(source),
            base,
            client,
            credentials,
            vm,
            resources,
        );
        Ok(key)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn parsed_response(
        &mut self,
        key: ModuleKey,
        source: ModuleSource,
        base: Arc<Url>,
        client: &Arc<Url>,
        credentials: CredentialsMode,
        vm: &mut Vm,
        resources: &mut ResourceQueue,
    ) {
        let result = (|| -> Result<_, String> {
            self.charge_url(&base)?;
            let len = match &source {
                ModuleSource::JavaScript(text) => text.len(),
                ModuleSource::Typed(bytes) => bytes.len(),
            };
            if len > 1024 * 1024 || self.source_bytes + len > MAX_MODULE_SOURCE_BYTES {
                return Err("Module source byte budget exceeded".into());
            }
            self.source_bytes += len;
            let id = match source {
                ModuleSource::JavaScript(text) if key.1 == ModuleType::JavaScript => {
                    let external = self.requests.values().any(|entry| entry.0 == key.0);
                    self.sources.capture(None, external.then_some(base.as_str()), || format!("Module #{}", key.0 + 1), &text);
                    vm.parse_module(&text, base.as_str())
                }
                ModuleSource::Typed(bytes) if key.1 != ModuleType::JavaScript => vm.parse_module_bytes(&bytes, base.as_str(), key.1),
                _ => return Err("Module payload does not match the requested type".into()),
            }.map_err(|error| error.to_string())?;
            let requests = vm.module_requests(id).map_err(|e| e.to_string())?;
            if self.edges + requests.len() > MAX_MODULE_EDGES {
                return Err("Module graph dependency budget exceeded".into());
            }
            self.edges += requests.len();
            let mut edges = Vec::with_capacity(requests.len());
            for request in requests {
                let specifier = request.specifier.to_string_lossy();
                let url = self.resolve_specifier(&specifier, &base)?;
                let dep = self.external_typed(
                    url,
                    client,
                    credentials,
                    None,
                    resources,
                    request.module_type().map_err(|e| e.to_string())?,
                )?;
                edges.push((request, dep));
            }
            vm.root_module(id).map_err(|e| e.to_string())?;
            Ok((id, edges))
        })();
        match result {
            Ok((id, edges)) => {
                self.ids.insert(id, key);
                self.records[key.0] = Record::Parsed {
                    id,
                    edges,
                    credentials,
                };
            }
            Err(error) => self.fail(key, error),
        }
    }

    pub(super) fn fail(&mut self, key: ModuleKey, error: String) {
        self.records[key.0] = Record::Failed(error.into());
    }

    pub(super) fn readiness(&self, root: ModuleKey) -> Readiness {
        let mut work = vec![root];
        let mut visited = HashSet::new();
        let mut pending = false;
        while let Some(key) = work.pop() {
            if !visited.insert(key.0) {
                continue;
            }
            match &self.records[key.0] {
                Record::Loading => pending = true,
                Record::Failed(error) => return Readiness::Failed(error.clone()),
                Record::Parsed { edges, .. } => {
                    work.extend(edges.iter().rev().map(|(_, key)| *key))
                }
            }
        }
        match &self.records[root.0] {
            Record::Parsed { id, .. } if !pending => Readiness::Ready(*id),
            _ => Readiness::Pending,
        }
    }

    pub(super) fn link(&self, vm: &mut Vm, root: ModuleId) -> Result<(), JsError> {
        vm.link_module_with_requests(root, |_, parent, request| {
            if let Some(key) = self.ids.get(&parent)
                && let Record::Parsed { edges, .. } = &self.records[key.0]
                && let Some((_, target)) = edges.iter().find(|(name, _)| name == request)
                && let Record::Parsed { id, .. } = &self.records[target.0]
            {
                Ok(*id)
            } else {
                Err(JsError::type_error(
                    "Module dependency missing from a linked graph",
                ))
            }
        })
    }
}
