//! Borrowed DOM changes shared between the parser and the script host.
use super::{AttributeNamespace, Document, NodeId};

pub(crate) type MutationSink = Box<dyn for<'a> FnMut(&Document, Mutation<'a>)>;

pub(crate) enum Mutation<'a> {
    Children {
        target: NodeId,
        added: &'a [NodeId],
        removed: &'a [NodeId],
        previous: Option<NodeId>,
        next: Option<NodeId>,
    },
    Attribute {
        target: NodeId,
        name: &'a str,
        namespace: Option<AttributeNamespace>,
        old: Option<&'a str>,
    },
    CharacterData {
        target: NodeId,
        old: &'a str,
    },
    /// Recorded before losing ancestry, including observer-suppressed removes.
    Removing {
        parent: NodeId,
        child: NodeId,
    },
}

pub(crate) fn siblings(
    doc: &Document,
    parent: NodeId,
    node: NodeId,
) -> (Option<NodeId>, Option<NodeId>) {
    let children = &doc.nodes[parent.0].children;
    children
        .iter()
        .position(|&child| child == node)
        .map_or((None, None), |index| {
            (
                index.checked_sub(1).map(|previous| children[previous]),
                children.get(index + 1).copied(),
            )
        })
}
