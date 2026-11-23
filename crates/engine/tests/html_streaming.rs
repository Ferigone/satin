use rust_browser_engine::{
    dom::{Document, Namespace, NodeId, NodeKind},
    html::{
        self, HtmlStream, HtmlStreamError, ParserEvent, ParserScript, ParserScriptKind, WriteId,
    },
};
use std::collections::BTreeMap;

fn to_script(parser: &mut HtmlStream) -> ParserScript {
    loop {
        match parser.next_event().unwrap() {
            ParserEvent::Script(script) => return script,
            ParserEvent::Progress(_) => {}
            event => panic!("expected script, got {event:?}"),
        }
    }
}
fn to_write(parser: &mut HtmlStream, target: WriteId) {
    loop {
        match parser.next_event().unwrap() {
            ParserEvent::WriteComplete(id) if id == target => return,
            ParserEvent::Progress(_) | ParserEvent::WriteComplete(_) => {}
            event => panic!("expected write boundary {target:?}, got {event:?}"),
        }
    }
}
fn complete(parser: &mut HtmlStream) {
    loop {
        match parser.next_event().unwrap() {
            ParserEvent::Complete => return,
            ParserEvent::Script(script) => parser.skip_script(script.node).unwrap(),
            ParserEvent::Progress(_) | ParserEvent::WriteComplete(_) => {}
            event => panic!("expected completion, got {event:?}"),
        }
    }
}
fn id(doc: &Document, name: &str) -> NodeId {
    doc.connected_nodes()
        .find(|&id| doc.attr(id, "id") == Some(name))
        .unwrap()
}
fn has(doc: &Document, name: &str) -> bool {
    doc.connected_nodes()
        .any(|id| doc.attr(id, "id") == Some(name))
}
fn static_stream(chunks: &[&str], scripting: bool) -> Document {
    let mut parser = HtmlStream::new(scripting);
    for chunk in chunks {
        parser.feed(chunk).unwrap();
        loop {
            match parser.next_event().unwrap() {
                ParserEvent::NeedInput => break,
                ParserEvent::Progress(_) => {}
                ParserEvent::Script(script) => parser.skip_script(script.node).unwrap(),
                event => panic!("unexpected chunk event {event:?}"),
            }
        }
    }
    parser.finish().unwrap();
    complete(&mut parser);
    parser.take_document().unwrap()
}
fn annotated(doc: &Document) -> Vec<(Namespace, String, Option<String>)> {
    let mut result = vec![];
    let mut pending = vec![doc.root];
    while let Some(id) = pending.pop() {
        if let NodeKind::Element { tag, attrs } = &doc.nodes[id.0].kind {
            result.push((
                doc.namespace(id),
                tag.clone(),
                attrs.get("viewBox").cloned(),
            ));
        }
        let children = doc.template_contents.get(&id).copied().unwrap_or(id);
        pending.extend(doc.nodes[children.0].children.iter().rev().copied());
    }
    result
}

#[test]
fn every_unicode_chunk_boundary_matches_one_shot_tree_construction() {
    for source in [
        "<!doctype html><title>A &amp; B &NotEqualTilde;</title><p a='x>y' b=unquoted>🦀&copy; &#x1f642; &notit;",
        "<!--x--!><!-- nested <!-->a<!--->b<?bogus>c</>tail",
        "<!DOCTYPE HTML PUBLIC 'p>q' 's'><p =abc='x' 'name>tail</p>",
        "<table>foo \n<tr><td>A<td>B</table>tail<b>x<i>y</b>z</i>",
        "<textarea>\r\n&amp;<>x</textareax>y</textarea><style>x<y &amp;</style><p>after",
        "<script><!-- <script>x</script> -->z</script><p>tail",
        "<script><!--> <script>x</script>after</script><noscript><p>x</noscript>",
        "<svg viewBox='0 0 10 10'><![CDATA[a]]x]]><g/><foreignObject><p>x</p></foreignObject></svg><math><mi><b>x</b></mi></math>",
        "<template><table><tr><td><b>x<template><svg><circle/></svg></template></template><p>tail",
        "<p>broken<foo attr='value",
        "<p>&#12345678901234567890123456789012345678901234567890",
        "<plaintext><b>literal &amp;\0 text",
        "<svg><![CDATA[a\0]]",
        "<script>unterminated</scr",
        "<p>\r\nA\rB\nC</p></",
    ] {
        for enabled in [false, true] {
            let reference = html::parse_with_scripting(source, enabled);
            for split in (0..=source.len()).filter(|&split| source.is_char_boundary(split)) {
                let doc = static_stream(&[&source[..split], &source[split..]], enabled);
                assert_eq!(
                    doc.serialize(),
                    reference.serialize(),
                    "split {split}, scripting {enabled}: {source:?}"
                );
                assert_eq!(doc.quirks_mode, reference.quirks_mode);
                assert_eq!(annotated(&doc), annotated(&reference));
                assert!(!doc.truncated);
            }
            let chars = source
                .char_indices()
                .map(|(start, c)| &source[start..start + c.len_utf8()])
                .collect::<Vec<_>>();
            assert_eq!(
                static_stream(&chars, enabled).serialize(),
                reference.serialize(),
                "one-code-point chunks: {source:?}"
            );
        }
    }
}

#[test]
fn first_script_observes_only_the_parsed_prefix_and_exact_script_identity() {
    let mut parser = HtmlStream::new(true);
    parser.feed("<body><div id=before>one</div><script id=first>first()</script><div id=after>two</div>").unwrap();
    parser.finish().unwrap();
    let script = to_script(&mut parser);
    let doc = parser.document().unwrap();
    assert!(has(doc, "before"));
    assert!(!has(doc, "after"));
    assert_eq!(doc.attr(script.node, "id"), Some("first"));
    assert_eq!(doc.text(script.node), "first()");
    assert!(parser.is_parser_inserted_script(script.node));
    assert_eq!(script.kind, ParserScriptKind::ClassicBlocking);
    parser.begin_script(script.node).unwrap();
    assert_eq!(
        parser.next_event().unwrap(),
        ParserEvent::Paused {
            script: script.node
        }
    );
    parser.resume_script(script.node).unwrap();
    complete(&mut parser);
    assert!(has(parser.document().unwrap(), "after"));
}

#[test]
fn nested_inline_writes_finish_synchronously_before_original_input() {
    let mut parser = HtmlStream::new(true);
    parser
        .feed("<body><script id=outer>outer()</script><p id=after>after</p>")
        .unwrap();
    parser.finish().unwrap();
    let outer = to_script(&mut parser);
    parser.begin_script(outer.node).unwrap();
    let first = parser
        .write("<p id=written>one<script id=inner>inner()</script>tail")
        .unwrap();
    let inner = to_script(&mut parser);
    assert_eq!(inner.depth, 2);
    assert!(!has(parser.document().unwrap(), "after"));
    parser.begin_script(inner.node).unwrap();
    let nested = parser.write("<b id=nested>nested</b>").unwrap();
    to_write(&mut parser, nested);
    assert_eq!(
        parser
            .document()
            .unwrap()
            .text(id(parser.document().unwrap(), "nested")),
        "nested"
    );
    parser.resume_script(inner.node).unwrap();
    to_write(&mut parser, first);
    assert!(!has(parser.document().unwrap(), "after"));
    let second = parser.write("<i id=last>last</i>").unwrap();
    to_write(&mut parser, second);
    assert_eq!(
        parser.next_event().unwrap(),
        ParserEvent::Paused { script: outer.node }
    );
    parser.resume_script(outer.node).unwrap();
    complete(&mut parser);
    assert_eq!(
        parser.document().unwrap().serialize(),
        "<html><head></head><body><script id=\"outer\">outer()</script><p id=\"written\">one<script id=\"inner\">inner()</script><b id=\"nested\">nested</b>tail<i id=\"last\">last</i></p><p id=\"after\">after</p></body></html>"
    );
}

#[test]
fn write_markers_preserve_partial_tags_references_and_input_eof() {
    let mut parser = HtmlStream::new(true);
    parser.feed("<script>x</script>").unwrap();
    parser.finish().unwrap();
    let script = to_script(&mut parser);
    parser.begin_script(script.node).unwrap();
    let first = parser.write("<p id='sp").unwrap();
    to_write(&mut parser, first);
    assert!(!has(parser.document().unwrap(), "split"));
    let second = parser.write("lit'>A &cop").unwrap();
    to_write(&mut parser, second);
    let stable = id(parser.document().unwrap(), "split");
    assert_eq!(parser.document().unwrap().text(stable), "A ");
    let third = parser.write("y; B\r").unwrap();
    to_write(&mut parser, third);
    let fourth = parser.write("\nC</p>").unwrap();
    to_write(&mut parser, fourth);
    assert_eq!(parser.document().unwrap().text(stable), "A © B\nC");
    parser.resume_script(script.node).unwrap();
    complete(&mut parser);
    assert_eq!(id(parser.document().unwrap(), "split"), stable);
    assert_eq!(parser.write("x"), Err(HtmlStreamError::NoInsertionPoint));
}

#[test]
fn a_written_comment_changes_the_unread_network_tokenization_state() {
    let mut parser = HtmlStream::new(true);
    parser
        .feed("<script>x</script><p id=after>network</p>")
        .unwrap();
    parser.finish().unwrap();
    let script = to_script(&mut parser);
    parser.begin_script(script.node).unwrap();
    let write = parser.write("<!--").unwrap();
    to_write(&mut parser, write);
    parser.resume_script(script.node).unwrap();
    complete(&mut parser);
    assert!(!has(parser.document().unwrap(), "after"));
    assert!(
        parser
            .document()
            .unwrap()
            .serialize()
            .contains("<!--<p id=after>network</p>-->")
    );
}

#[test]
fn nested_external_script_waits_for_outer_return_and_retains_all_queued_writes() {
    let mut parser = HtmlStream::new(true);
    parser
        .feed("<body><script id=outer>x</script><p id=network>network</p>")
        .unwrap();
    parser.finish().unwrap();
    let outer = to_script(&mut parser);
    parser.begin_script(outer.node).unwrap();
    let first = parser
        .write(
            "<div id=first>A<script id=external src='local.js'></script><b id=rest>rest</b></div>",
        )
        .unwrap();
    let external = to_script(&mut parser);
    assert!(external.external);
    assert_eq!(external.depth, 2);
    assert_eq!(
        parser.begin_script(external.node),
        Err(HtmlStreamError::NestedExternalPending)
    );
    assert!(!has(parser.document().unwrap(), "rest"));
    let second = parser.write("<p id=second>second</p>").unwrap();
    parser.resume_script(outer.node).unwrap();
    let pending = to_script(&mut parser);
    assert_eq!(pending.node, external.node);
    assert_eq!(pending.depth, 1);
    parser.begin_script(external.node).unwrap();
    let nested = parser.write("<i id=external-write>written</i>").unwrap();
    to_write(&mut parser, nested);
    parser.resume_script(external.node).unwrap();
    to_write(&mut parser, first);
    to_write(&mut parser, second);
    complete(&mut parser);
    let doc = parser.document().unwrap();
    assert!(has(doc, "rest") && has(doc, "second") && has(doc, "network"));
    let order = doc
        .connected_nodes()
        .filter_map(|node| doc.attr(node, "id"))
        .collect::<Vec<_>>();
    assert_eq!(
        order,
        vec![
            "outer",
            "first",
            "external",
            "external-write",
            "rest",
            "second",
            "network"
        ]
    );
}

#[test]
fn leased_dom_mutations_keep_open_element_ids_and_detached_insertion_targets() {
    let mut parser = HtmlStream::new(true);
    parser.feed("<body><div id=removed><script>x</script><p id=inside>inside</p></div><p id=tail>tail</p>").unwrap();
    parser.finish().unwrap();
    let script = to_script(&mut parser);
    parser.begin_script(script.node).unwrap();
    let mut doc = parser.take_document().unwrap();
    let removed = id(&doc, "removed");
    let body = doc.nodes[removed.0].parent.unwrap();
    assert_eq!(parser.next_event(), Err(HtmlStreamError::DocumentLeased));
    assert!(parser.document().is_none());
    doc.nodes[body.0].children.retain(|&node| node != removed);
    doc.nodes[removed.0].parent = None;
    let added = doc.append(
        body,
        NodeKind::Element {
            tag: "aside".into(),
            attrs: BTreeMap::from([("id".into(), "added".into())]),
        },
    );
    parser.restore_document(doc).unwrap();
    parser.resume_script(script.node).unwrap();
    complete(&mut parser);
    let doc = parser.document().unwrap();
    assert_eq!(id(doc, "added"), added);
    assert!(has(doc, "tail"));
    assert!(!has(doc, "inside"));
    assert!(
        doc.nodes[removed.0]
            .children
            .iter()
            .any(|&node| doc.attr(node, "id") == Some("inside"))
    );
    assert_eq!(doc.attr(script.node, "id"), None);
    assert_eq!(doc.tag(script.node), "script");
}

#[test]
fn formatting_reconstruction_uses_original_start_tags_after_a_script_mutates_attrs() {
    let mut parser = HtmlStream::new(true);
    parser
        .feed("<p><b id=original><script>x</script></p>tail</b>")
        .unwrap();
    parser.finish().unwrap();
    let script = to_script(&mut parser);
    parser.begin_script(script.node).unwrap();
    let mut doc = parser.take_document().unwrap();
    let original = id(&doc, "original");
    let NodeKind::Element { attrs, .. } = &mut doc.nodes[original.0].kind else {
        unreachable!()
    };
    attrs.insert("id".into(), "changed".into());
    parser.restore_document(doc).unwrap();
    parser.resume_script(script.node).unwrap();
    complete(&mut parser);
    let doc = parser.document().unwrap();
    assert_eq!(id(doc, "changed"), original);
    assert_ne!(id(doc, "original"), original);
    assert_eq!(doc.text(id(doc, "original")), "tail");
}

#[test]
fn noahs_ark_compares_tokens_even_when_the_script_changes_one_matching_element() {
    let mut parser = HtmlStream::new(true);
    parser
        .feed("<p><font size=4><font size=4><font size=4><script>x</script><font size=4><p>X")
        .unwrap();
    parser.finish().unwrap();
    let script = to_script(&mut parser);
    parser.begin_script(script.node).unwrap();
    let mut doc = parser.take_document().unwrap();
    let third = doc.elements("font").nth(2).unwrap();
    let NodeKind::Element { attrs, .. } = &mut doc.nodes[third.0].kind else {
        unreachable!()
    };
    attrs.insert("size".into(), "5".into());
    parser.restore_document(doc).unwrap();
    parser.resume_script(script.node).unwrap();
    complete(&mut parser);
    let doc = parser.document().unwrap();
    let sizes = doc
        .elements("font")
        .map(|node| doc.attr(node, "size").unwrap())
        .collect::<Vec<_>>();
    assert_eq!(sizes, ["4", "4", "5", "4", "4", "4", "4"]);
}

#[test]
fn invalid_leases_stop_without_indexing_removed_arena_nodes() {
    let mut parser = HtmlStream::new(true);
    parser.feed("<body><div><script>x</script></div>").unwrap();
    let _ = to_script(&mut parser);
    let mut doc = parser.take_document().unwrap();
    doc.nodes.truncate(1);
    assert_eq!(
        parser.restore_document(doc),
        Err(HtmlStreamError::InvalidDocument)
    );
    assert_eq!(parser.next_event().unwrap(), ParserEvent::Truncated);
    assert!(parser.take_document().unwrap().truncated);
}

#[test]
fn script_in_removed_open_subtree_is_not_prepared_but_keep_its_parser_identity() {
    let mut parser = HtmlStream::new(true);
    parser.feed("<body><div id=removed><script>x</script><script id=detached>never</script></div><script id=after>yes</script>").unwrap();
    parser.finish().unwrap();
    let first = to_script(&mut parser);
    parser.begin_script(first.node).unwrap();
    let mut doc = parser.take_document().unwrap();
    let removed = id(&doc, "removed");
    let parent = doc.nodes[removed.0].parent.take().unwrap();
    doc.nodes[parent.0].children.retain(|&node| node != removed);
    parser.restore_document(doc).unwrap();
    parser.resume_script(first.node).unwrap();
    let after = to_script(&mut parser);
    let doc = parser.document().unwrap();
    assert_eq!(doc.attr(after.node, "id"), Some("after"));
    let detached = doc.nodes[removed.0]
        .children
        .iter()
        .copied()
        .find(|&node| doc.attr(node, "id") == Some("detached"))
        .unwrap();
    assert!(parser.is_parser_inserted_script(detached));
}

#[test]
fn classic_attributes_data_modules_and_inert_templates_are_classified_at_end_tags() {
    let source = "<template><script>inert()</script></template><svg><script>foreign()</script></svg><script type=application/json>{}</script><script type=module>module</script><script type=importmap>{}</script><script src=a defer></script><script src=b async defer></script><script defer>inline()</script><script language=vbscript>data</script>";
    let mut parser = HtmlStream::new(true);
    parser.feed(source).unwrap();
    parser.finish().unwrap();
    let mut kinds = vec![];
    loop {
        match parser.next_event().unwrap() {
            ParserEvent::Script(script) => {
                kinds.push(script.kind);
                parser.skip_script(script.node).unwrap();
            }
            ParserEvent::Progress(_) => {}
            ParserEvent::Complete => break,
            event => panic!("{event:?}"),
        }
    }
    assert_eq!(
        kinds,
        vec![
            ParserScriptKind::Data,
            ParserScriptKind::Module,
            ParserScriptKind::ImportMap,
            ParserScriptKind::ClassicDefer,
            ParserScriptKind::ClassicAsync,
            ParserScriptKind::ClassicBlocking,
            ParserScriptKind::Data
        ]
    );
    let doc = parser.document().unwrap();
    let template = doc.elements("template").next().unwrap();
    let content = doc.template_contents[&template];
    assert!(
        doc.nodes[content.0]
            .children
            .iter()
            .any(|&node| doc.tag(node) == "script")
    );
}

#[test]
fn progress_exposes_resources_and_closed_style_before_later_markup_is_built() {
    let mut parser = HtmlStream::new(true);
    parser
        .feed("<link rel=stylesheet href=a.css><style>p{color:red}</style><img id=later src=b.png>")
        .unwrap();
    parser.finish().unwrap();
    let mut saw_link = false;
    let mut saw_style = false;
    loop {
        match parser.next_event().unwrap() {
            ParserEvent::Progress(progress) => {
                let doc = parser.document().unwrap();
                if progress
                    .new_nodes
                    .clone()
                    .any(|index| doc.tag(NodeId(index)) == "link")
                {
                    saw_link = true;
                    assert!(!has(doc, "later"));
                }
                if let Some(node) = progress.closed
                    && doc.tag(node) == "style"
                {
                    saw_style = true;
                    assert_eq!(doc.text(node), "p{color:red}");
                    assert!(!has(doc, "later"));
                }
            }
            ParserEvent::Complete => break,
            event => panic!("{event:?}"),
        }
    }
    assert!(saw_link && saw_style);
}

#[test]
fn eof_closes_raw_resource_text_without_preparing_an_unclosed_script() {
    for tag in ["style", "script"] {
        let mut parser = HtmlStream::new(true);
        parser.feed(&format!("<{tag}>source")).unwrap();
        parser.finish().unwrap();
        let mut closed = false;
        loop {
            match parser.next_event().unwrap() {
                ParserEvent::Progress(progress) => {
                    if let Some(node) = progress.closed {
                        let doc = parser.document().unwrap();
                        assert_eq!(doc.tag(node), tag);
                        assert_eq!(doc.text(node), "source");
                        closed = true;
                    }
                }
                ParserEvent::Complete => break,
                event => panic!("unexpected EOF event {event:?}"),
            }
        }
        assert!(closed);
    }
}

#[test]
fn long_unfinished_comment_yields_progress_without_emitting_a_token() {
    let mut parser = HtmlStream::new(false);
    let comment = "x".repeat(html::STREAM_ADVANCE_BYTES * 3);
    parser.feed(&format!("<!--{comment}")).unwrap();
    for _ in 0..3 {
        let ParserEvent::Progress(progress) = parser.next_event().unwrap() else {
            panic!("long token should yield")
        };
        assert!(progress.new_nodes.is_empty());
        assert_eq!(parser.document().unwrap().nodes.len(), 1);
    }
    assert_eq!(parser.next_event().unwrap(), ParserEvent::NeedInput);
    parser.feed("--><p>after").unwrap();
    parser.finish().unwrap();
    complete(&mut parser);
    assert!(
        matches!(&parser.document().unwrap().nodes[1].kind, NodeKind::Comment(value) if value == &comment)
    );
}

#[test]
fn repeated_foster_insertions_hit_work_budget_before_large_quadratic_sibling_scans() {
    let mut parser = HtmlStream::new(false);
    parser
        .feed(&format!("<table>{}</table>", "<i></i>".repeat(20_000)))
        .unwrap();
    parser.finish().unwrap();
    loop {
        match parser.next_event() {
            Ok(ParserEvent::Progress(_)) => {}
            Ok(ParserEvent::Truncated) | Err(HtmlStreamError::ResourceLimit) => break,
            event => panic!("expected work cap, got {event:?}"),
        }
    }
    let doc = parser.document().unwrap();
    assert!(doc.truncated && doc.nodes.len() < html::MAX_NODES);
    assert!(parser.report().work <= html::MAX_STREAM_WORK);
    for id in doc.connected_nodes() {
        for &child in &doc.nodes[id.0].children {
            assert_eq!(doc.nodes[child.0].parent, Some(id));
        }
    }
}

#[test]
fn script_nesting_budget_unwinds_all_existing_insertion_points() {
    let mut parser = HtmlStream::new(true);
    parser.feed("<script>x</script>").unwrap();
    parser.finish().unwrap();
    let mut active = vec![];
    for _ in 0..html::MAX_SCRIPT_NESTING {
        let script = to_script(&mut parser);
        parser.begin_script(script.node).unwrap();
        active.push(script.node);
        parser.write("<script>x</script>").unwrap();
    }
    let over = to_script(&mut parser);
    assert_eq!(
        parser.begin_script(over.node),
        Err(HtmlStreamError::ResourceLimit)
    );
    for node in active.into_iter().rev() {
        parser.resume_script(node).unwrap();
    }
    assert_eq!(parser.script_nesting(), 0);
    assert_eq!(parser.next_event().unwrap(), ParserEvent::Truncated);
}

#[test]
fn bounded_streaming_and_reentrant_state_drop_on_a_one_mib_thread_stack() {
    std::thread::Builder::new()
        .stack_size(1024 * 1024)
        .spawn(|| {
            let source = format!(
                "<body>{}<script>x</script>{}",
                "<div>".repeat(100),
                "</div>".repeat(100)
            );
            let mut parser = HtmlStream::new(true);
            for c in source.chars() {
                parser.feed(&c.to_string()).unwrap();
                loop {
                    match parser.next_event().unwrap() {
                        ParserEvent::NeedInput => break,
                        ParserEvent::Script(script) => parser.skip_script(script.node).unwrap(),
                        ParserEvent::Progress(_) => {}
                        event => panic!("{event:?}"),
                    }
                }
            }
            parser.finish().unwrap();
            complete(&mut parser);
            assert!(!parser.report().truncated);
            let mut parser = HtmlStream::new(true);
            parser.feed("<script>x</script>").unwrap();
            parser.finish().unwrap();
            let script = to_script(&mut parser);
            parser.begin_script(script.node).unwrap();
            for _ in 0..html::MAX_WRITE_CALLS {
                let write = parser.write("").unwrap();
                to_write(&mut parser, write);
            }
            assert_eq!(parser.write(""), Err(HtmlStreamError::ResourceLimit));
            assert!(parser.document().unwrap().truncated);
            let mut parser = HtmlStream::new(false);
            assert_eq!(
                parser.feed(&"x".repeat(html::MAX_INPUT_BYTES + 1)),
                Err(HtmlStreamError::ResourceLimit)
            );
            assert_eq!(parser.next_event().unwrap(), ParserEvent::Truncated);
        })
        .unwrap()
        .join()
        .unwrap();
}
