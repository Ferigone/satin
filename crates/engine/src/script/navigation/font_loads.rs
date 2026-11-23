//! CSS descriptors retain stylesheet source order, while sources are tried
//! in declared order and shared across descriptors within this document.
use super::{
    Consumer, ResourceQueue,
    parser_host::{resolve_resource, resource_request},
};
use crate::{
    css,
    dom::NodeId,
    fonts::{
        FontError, FontFace, FontFaceEntry, FontFaceRule, FontFormat, FontSet, FontSource,
        MAX_FONT_FACES, MAX_FONT_SET_BYTES,
    },
    loader::ResourceKind,
    network::CredentialsMode,
};
use std::{collections::HashMap, sync::Arc};
use url::Url;

#[derive(Clone, Eq, PartialEq, Hash)]
pub(super) enum FontKey {
    Remote(Arc<Url>),
    Local(Arc<str>),
}
enum Attempt {
    Waiting(Vec<usize>),
    Loaded(FontFace),
    Failed,
}
struct FontJob {
    position: (usize, usize),
    base: Arc<Url>,
    rule: FontFaceRule,
    next_source: usize,
    face: Option<FontFace>,
}
#[derive(Default)]
pub(super) struct FontLoads {
    jobs: Vec<FontJob>,
    attempts: HashMap<FontKey, Attempt>,
    bytes: usize,
    dirty: bool,
}
impl FontLoads {
    pub(super) fn register(
        &mut self,
        stylesheet: NodeId,
        source: &str,
        base: Arc<Url>,
        viewport_width: f32,
        client: &Arc<Url>,
        resources: &mut ResourceQueue,
    ) {
        for (index, rule) in css::font_faces(source, viewport_width)
            .into_iter()
            .enumerate()
        {
            if self.jobs.len() >= MAX_FONT_FACES {
                break;
            }
            let job = self.jobs.len();
            self.jobs.push(FontJob {
                position: (stylesheet.0, index),
                base: base.clone(),
                rule,
                next_source: 0,
                face: None,
            });
            self.next(job, client, resources);
        }
    }

    fn next(&mut self, job: usize, client: &Arc<Url>, resources: &mut ResourceQueue) {
        let entry = &mut self.jobs[job];
        while let Some(source) = entry.rule.sources.get(entry.next_source) {
            entry.next_source += 1;
            let key = match source {
                FontSource::Local(name) => FontKey::Local(name.clone()),
                FontSource::Url {
                    format: Some(FontFormat::Unsupported),
                    ..
                } => continue,
                FontSource::Url { url, .. } => {
                    let Ok(url) = resolve_resource(client, &entry.base, url) else {
                        continue;
                    };
                    if !matches!(url.scheme(), "http" | "https" | "data") {
                        continue;
                    }
                    FontKey::Remote(Arc::new(url))
                }
            };
            match self.attempts.get_mut(&key) {
                Some(Attempt::Waiting(waiters)) => {
                    waiters.push(job);
                    return;
                }
                Some(Attempt::Loaded(face)) => {
                    entry.face = Some(face.clone());
                    self.dirty = true;
                    return;
                }
                Some(Attempt::Failed) => continue,
                None => {}
            }
            let (url, kind) = match &key {
                FontKey::Remote(url) => ((**url).clone(), ResourceKind::Font),
                FontKey::Local(name) => (
                    Url::parse("about:local-font").unwrap(),
                    ResourceKind::LocalFont(name.clone()),
                ),
            };
            if self.attempts.len() >= 128 {
                return;
            }
            let request = resource_request(
                url,
                client,
                kind,
                Some(CredentialsMode::SameOrigin),
                None,
                resources.deadline(),
            );
            if resources
                .queue(request, Consumer::Font(key.clone()))
                .is_ok()
            {
                self.attempts.insert(key, Attempt::Waiting(vec![job]));
                return;
            }
        }
    }

    pub(super) fn admit(
        &mut self,
        key: &FontKey,
        face: FontFace,
        client: &Arc<Url>,
        resources: &mut ResourceQueue,
    ) -> bool {
        if self.bytes.saturating_add(face.decoded_bytes()) > MAX_FONT_SET_BYTES {
            self.fail(key, client, resources);
            return false;
        }
        let Some(Attempt::Waiting(waiters)) = self.attempts.get_mut(key) else {
            return false;
        };
        let waiters = std::mem::take(waiters);
        self.bytes += face.decoded_bytes();
        self.attempts
            .insert(key.clone(), Attempt::Loaded(face.clone()));
        for job in waiters {
            self.jobs[job].face = Some(face.clone());
        }
        self.dirty = true;
        true
    }
    pub(super) fn fail(&mut self, key: &FontKey, client: &Arc<Url>, resources: &mut ResourceQueue) {
        if let Some(Attempt::Waiting(waiters)) = self.attempts.insert(key.clone(), Attempt::Failed)
        {
            for job in waiters {
                self.next(job, client, resources);
            }
        }
    }
    pub(super) fn take_set(&mut self) -> Result<Option<FontSet>, FontError> {
        if !std::mem::take(&mut self.dirty) {
            return Ok(None);
        }
        let mut entries = self
            .jobs
            .iter()
            .filter_map(|job| {
                job.face.as_ref().map(|face| {
                    (
                        job.position,
                        FontFaceEntry {
                            rule: job.rule.clone(),
                            face: face.clone(),
                        },
                    )
                })
            })
            .collect::<Vec<_>>();
        entries.sort_unstable_by_key(|(position, _)| *position);
        FontSet::new(entries.into_iter().map(|(_, entry)| entry).collect()).map(Some)
    }
}
