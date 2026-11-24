//! Per-document module map. Transport happens on workers; parsed records and
//! execution remain in the owning realm. A cache hit shares module *identity*.
use super::{Consumer, ResourceQueue, import_map::ImportMap};
use crate::{
    loader::{ResourceKind, ResourceRequest},
    network::CredentialsMode,
};
use rust_browser_javascript::{JsError, JsString, ModuleId, Vm};
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
pub(super) struct ModuleKey(usize);

enum Record {
    Loading,
    Parsed {
        id: ModuleId,
        edges: Vec<(JsString, ModuleKey)>,
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
    requests: HashMap<Arc<Url>, ModuleKey>,
    ids: HashMap<ModuleId, ModuleKey>,
    url_bytes: usize,
    source_bytes: usize,
    edges: usize,
}
impl ModuleGraph {
    fn allocate(&mut self) -> Result<ModuleKey, String> {
        if self.records.len() >= MAX_MODULES {
            return Err("Module graph exceeds 512 records".into());
        }
        let key = ModuleKey(self.records.len());
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
        if let Some(key) = self.requests.get(&url) {
            return Ok(*key);
        }
        self.charge_url(&url)?;
        let key = self.allocate()?;
        let integrity = integrity.or_else(|| self.imports.integrity(&url));
        let request = ResourceRequest {
            url: url.clone(),
            client: client.clone(),
            kind: ResourceKind::ModuleScript,
            cors: Some(credentials),
            integrity,
            deadline: ResourceQueue::queue_deadline(),
        };
        self.requests.insert(Arc::new(url), key);
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
        let key = self.allocate()?;
        self.parsed_response(key, source, base, client, credentials, vm, resources);
        Ok(key)
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) fn parsed_response(
        &mut self,
        key: ModuleKey,
        source: String,
        base: Arc<Url>,
        client: &Arc<Url>,
        credentials: CredentialsMode,
        vm: &mut Vm,
        resources: &mut ResourceQueue,
    ) {
        let result = (|| -> Result<_, String> {
            self.charge_url(&base)?;
            if source.len() > 1024 * 1024
                || self.source_bytes + source.len() > MAX_MODULE_SOURCE_BYTES
            {
                return Err("Module source byte budget exceeded".into());
            }
            self.source_bytes += source.len();
            let external = self.requests.values().any(|entry| entry.0 == key.0);
            self.sources.capture(
                None,
                external.then_some(base.as_str()),
                || format!("Module #{}", key.0 + 1),
                &source,
            );
            let id = vm
                .parse_module(&source, base.as_str())
                .map_err(|e| e.to_string())?;
            let requests = vm.module_requests(id).map_err(|e| e.to_string())?;
            if self.edges + requests.len() > MAX_MODULE_EDGES {
                return Err("Module graph dependency budget exceeded".into());
            }
            self.edges += requests.len();
            let mut edges = Vec::with_capacity(requests.len());
            for request in requests {
                let specifier = request.specifier.to_string_lossy();
                let url = self.resolve_specifier(&specifier, &base)?;
                let dep = self.external(url, client, credentials, None, resources)?;
                edges.push((request.specifier, dep));
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
        vm.link_module(root, |_, parent, specifier| {
            if let Some(key) = self.ids.get(&parent)
                && let Record::Parsed { edges, .. } = &self.records[key.0]
                && let Some((_, target)) = edges.iter().find(|(name, _)| name == specifier)
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
