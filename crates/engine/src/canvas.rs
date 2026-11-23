use crate::dom::{Document, Namespace, NodeId};
use image::RgbaImage;
use std::{
    collections::HashMap,
    sync::{Arc, RwLock},
};

pub(crate) const DIMENSIONS: [(&str, u32); 2] = [("width", 300), ("height", 150)];

pub(crate) fn is_canvas(document: &Document, node: NodeId) -> bool {
    document.namespace(node) == Namespace::Html && document.tag(node) == "canvas"
}

pub(crate) fn dimension(document: &Document, node: NodeId, name: &str, default: u32) -> u32 {
    if document.attribute_namespace(node, name).is_some() {
        return default;
    }
    document
        .attr(node, name)
        .and_then(|value| {
            let value = value.trim_start_matches(['\t', '\n', '\x0c', '\r', ' ']);
            let (negative, digits) = match value.as_bytes().first() {
                Some(b'-') => (true, &value[1..]),
                Some(b'+') => (false, &value[1..]),
                _ => (false, value),
            };
            let end = digits.bytes().take_while(u8::is_ascii_digit).count();
            let number = digits[..end].parse::<i32>().ok()?;
            (!negative || number == 0).then_some(number as u32)
        })
        .unwrap_or(default)
}

pub(crate) fn size(document: &Document, node: NodeId) -> [u32; 2] {
    DIMENSIONS.map(|(name, default)| dimension(document, node, name, default))
}

#[derive(Clone, Debug)]
pub(crate) enum Frame {
    Bitmap(Arc<RgbaImage>),
    OpaqueBlack,
}

/// A layout keeps the positions of canvases. Painting reads their most recent
/// completed frames without rebuilding the layout or borrowing a script realm.
#[derive(Clone, Debug, Default)]
pub(crate) struct Frames(Arc<RwLock<HashMap<NodeId, Frame>>>);

impl Frames {
    pub(crate) fn get(&self, node: NodeId) -> Option<Frame> {
        self.0
            .read()
            .unwrap_or_else(|error| error.into_inner())
            .get(&node)
            .cloned()
    }

    pub(crate) fn update(&self, frames: impl Iterator<Item = (NodeId, Option<Frame>)>) {
        let mut map = self.0.write().unwrap_or_else(|error| error.into_inner());
        for (node, frame) in frames {
            if let Some(frame) = frame {
                map.insert(node, frame);
            } else {
                map.remove(&node);
            }
        }
    }
}
