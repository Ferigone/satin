//! Drive html5ever to the browser's existing resumable HTML parser boundaries.
use super::{
    MAX_INPUT_BYTES, ParserProgress,
    servo::{Handle, Sink, options},
    tree_builder::Tree,
};
use crate::dom::{NodeId, QuirksMode};
use html5ever::{
    TokenizerResult,
    buffer_queue::BufferQueue,
    tendril::StrTendril,
    tokenizer::{Token, TokenSink, TokenSinkResult, Tokenizer},
    tree_builder::{Tracer, TreeBuilder},
};
use std::{cell::RefCell, collections::VecDeque};

struct Hooks {
    tree: TreeBuilder<Handle, Sink>,
    progress: RefCell<VecDeque<ParserProgress>>,
}

impl TokenSink for Hooks {
    type Handle = Handle;

    fn process_token(&self, token: Token, line: u64) -> TokenSinkResult<Handle> {
        let tag = matches!(token, Token::TagToken(_));
        let sink = &self.tree.sink;
        let start = sink.tree.borrow().doc.nodes.len();
        sink.closed.set(None);
        if !sink.tree.borrow_mut().charge_work(1) {
            return TokenSinkResult::Continue;
        }
        let result = self.tree.process_token(token, line);
        let tree = sink.tree.borrow();
        let end = tree.doc.nodes.len();
        let closed = sink.closed.take();
        let executable_script = matches!(&result, TokenSinkResult::Script(handle)
            if handle.id().is_some_and(|id| tree.doc.scripting_enabled && tree.connected(id)));
        if start != end || closed.is_some() && !executable_script {
            self.progress.borrow_mut().push_back(ParserProgress {
                new_nodes: start..end,
                closed: if executable_script { None } else { closed },
            });
        }
        if tag && matches!(result, TokenSinkResult::Continue)
            && (tree.pending_element.is_some() || tree.halted)
        {
            // An encoding notification ends this feed without altering lexer state.
            // The response decoder already fixed the encoding before passing UTF-8.
            return TokenSinkResult::EncodingIndicator(StrTendril::new());
        }
        result
    }

    fn end(&self) {
        self.tree.end();
    }

    fn adjusted_current_node_present_but_not_in_html_namespace(&self) -> bool {
        self.tree.adjusted_current_node_present_but_not_in_html_namespace()
    }
}

pub(super) struct Parser {
    tokenizer: Tokenizer<Hooks>,
    input: BufferQueue,
}

impl Parser {
    pub(super) fn new(scripting: bool) -> Self {
        let options = options(scripting, QuirksMode::NoQuirks);
        let tree = TreeBuilder::new(Sink::new(scripting), options.tree_builder);
        Self {
            tokenizer: Tokenizer::new(
                Hooks { tree, progress: RefCell::new(VecDeque::new()) },
                options.tokenizer,
            ),
            input: BufferQueue::default(),
        }
    }

    pub(super) fn references(&self) -> Vec<NodeId> {
        struct LiveNodes(RefCell<Vec<NodeId>>);
        impl Tracer for LiveNodes {
            type Handle = Handle;
            fn trace_handle(&self, handle: &Handle) {
                if let Some(id) = handle.id() {
                    self.0.borrow_mut().push(id);
                }
            }
        }
        let nodes = LiveNodes(RefCell::new(Vec::new()));
        self.tokenizer.sink.tree.trace_handles(&nodes);
        let mut nodes = nodes.0.into_inner();
        nodes.sort_unstable();
        nodes.dedup();
        nodes
    }

    pub(super) fn pop_progress(&self) -> Option<ParserProgress> {
        self.tokenizer.sink.progress.borrow_mut().pop_front()
    }

    pub(super) fn progress_empty(&self) -> bool {
        self.tokenizer.sink.progress.borrow().is_empty()
    }

    pub(super) fn finish_custom(&self, original: NodeId, element: NodeId) {
        self.tokenizer.sink.tree.sink.finish_custom(original, element);
    }

    pub(super) fn feed(&self, tree: &mut Tree, text: &str) -> (Option<NodeId>, String) {
        self.input.push_back(StrTendril::from(text));
        let result = self.with_tree(tree, || self.tokenizer.feed(&self.input));
        let script = match &result {
            TokenizerResult::Script(handle) => handle.id().filter(|&id| {
                tree.doc.scripting_enabled && tree.connected(id)
            }),
            _ => None,
        };
        let mut remaining = String::new();
        if !matches!(result, TokenizerResult::Done) {
            while let Some(text) = self.input.pop_front() {
                remaining.push_str(&text);
            }
            if remaining.len() > MAX_INPUT_BYTES {
                tree.halted = true;
            }
        }
        (script, remaining)
    }

    pub(super) fn finish(&self, tree: &mut Tree) {
        self.with_tree(tree, || {
            // Any incomplete lookahead is held inside html5ever. End-of-file
            // processing resolves it without introducing a new input piece.
            self.tokenizer.end();
        });
    }

    fn with_tree<T>(&self, tree: &mut Tree, parse: impl FnOnce() -> T) -> T {
        let sink = &self.tokenizer.sink.tree.sink;
        std::mem::swap(tree, &mut *sink.tree.borrow_mut());
        let result = parse();
        std::mem::swap(tree, &mut *sink.tree.borrow_mut());
        result
    }
}
