use rust_browser_engine::{
    dom::{Document, NodeKind, QuirksMode},
    html,
};

fn assert_tree(document: &Document) {
    let mut seen = vec![false; document.nodes.len()];
    let mut pending = vec![document.root];
    assert!(document.nodes[document.root.0].parent.is_none());
    while let Some(id) = pending.pop() {
        assert!(!seen[id.0], "node visited twice: {id:?}");
        seen[id.0] = true;
        for &child in &document.nodes[id.0].children {
            assert!(child.0 < document.nodes.len());
            assert_eq!(document.nodes[child.0].parent, Some(id));
            pending.push(child);
        }
    }
    // Detached historical formatting nodes can remain in the arena, with stable
    // NodeIds. Their own parent/children links must still agree as well.
    let mut inbound = vec![0_u8; document.nodes.len()];
    for (index, node) in document.nodes.iter().enumerate() {
        for &child in &node.children {
            assert_eq!(document.nodes[child.0].parent.map(|id| id.0), Some(index));
            inbound[child.0] += 1;
            assert_eq!(inbound[child.0], 1);
        }
    }
    for (index, node) in document.nodes.iter().enumerate() {
        assert_eq!(inbound[index] == 1, node.parent.is_some());
    }
}

#[test]
fn malformed_sequences_preserve_arena_invariants() {
    // Deterministic grammar fuzzing, including adoption inside foster-parented
    // table content and UTF-8 attribute/text boundaries. No external RNG needed.
    let tokens = [
        "<p>",
        "</p>",
        "<div>",
        "</div>",
        "<b>",
        "</b>",
        "<i>",
        "</i>",
        "<a id=é>",
        "</a>",
        "<table>",
        "</table>",
        "<tr>",
        "</tr>",
        "<td>",
        "</td>",
        "<th>",
        "</th>",
        "<tbody>",
        "</tbody>",
        "<caption>",
        "</caption>",
        "<col>",
        "<colgroup>",
        "</colgroup>",
        "<li>",
        "</li>",
        "<ul>",
        "</ul>",
        "<dt>",
        "<dd>",
        "<dl>",
        "</dl>",
        "<button>",
        "</button>",
        "<select>",
        "</select>",
        "<option>",
        "<optgroup>",
        "<form>",
        "</form>",
        "<h1>",
        "</h2>",
        "<object>",
        "</object>",
        "<!--x--!>",
        "<!DOCTYPE html>",
        "</>",
        "</?x>",
        "<a/>",
        "<a =x q='a>b'>",
        "x",
        " ",
        "\n",
        "é🦀",
        "&notit;",
        "\0",
        "</span>",
        "<hr>",
    ];
    let mut state = 0x1234_abcd_u64;
    for case in 0..2048 {
        let mut source = String::new();
        for _ in 0..96 {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            source.push_str(tokens[state as usize % tokens.len()]);
        }
        let document = html::parse(&source);
        assert!(
            !document.truncated,
            "unexpected resource abort in small case {case}: {source:?}"
        );
        assert_tree(&document);
        let _ = document.serialize();
        let _ = document.text(document.root);
    }
}

#[test]
fn ignored_end_tags_do_not_recurse_on_the_native_stack() {
    let document = html::parse(&format!("{}done", "</>".repeat(100_000)));
    assert!(!document.truncated);
    assert_eq!(document.text(document.root), "done");
    assert_tree(&document);
    let document = html::parse("<pre></>\nx</pre><table> </>x<3<tr><td>cell");
    assert_eq!(document.text(document.elements("pre").next().unwrap()), "x");
    let table = document.elements("table").next().unwrap();
    let parent = document.nodes[table.0].parent.unwrap();
    let siblings = &document.nodes[parent.0].children;
    let index = siblings.iter().position(|&id| id == table).unwrap();
    assert!(
        matches!(&document.nodes[siblings[index - 1].0].kind, NodeKind::Text(text) if text == " x<3")
    );
}

#[test]
fn parser_budgets_are_explicit_and_leave_valid_trees() {
    let deep = html::parse(&format!("{}x", "<div>".repeat(2000)));
    assert!(deep.truncated);
    assert!(deep.nodes.len() < 1000);
    assert_tree(&deep);
    let _ = deep.serialize();
    let broad = html::parse(&"<br>".repeat(html::MAX_NODES + 10));
    assert!(broad.truncated);
    assert_eq!(broad.nodes.len(), html::MAX_NODES);
    assert_tree(&broad);
    let unicode = html::parse(&format!("<p>{}", "é".repeat(html::MAX_INPUT_BYTES / 2 + 2)));
    assert!(unicode.truncated);
    assert!(unicode.text(unicode.root).ends_with('é'));
    assert_tree(&unicode);
}

#[test]
fn raw_end_tag_boundaries_preprocessing_and_self_closing_html() {
    let document = html::parse(
        "<!doctype html><title> A\r\nB\rC&nbsp;D </title><div/>inside</div><textarea>\n&amp;<b>\0</textarea><style>a</stylex>b</STYLE><script><!--<script></script>-->done</script>",
    );
    assert_eq!(document.quirks_mode, QuirksMode::NoQuirks);
    assert_eq!(document.title(), "A B C\u{a0}D");
    assert_eq!(
        document.text(document.elements("div").next().unwrap()),
        "inside"
    );
    assert_eq!(
        document.text(document.elements("textarea").next().unwrap()),
        "&<b>�"
    );
    assert_eq!(
        document.text(document.elements("style").next().unwrap()),
        "a</stylex>b"
    );
    assert_eq!(
        document.text(document.elements("script").next().unwrap()),
        "<!--<script></script>-->done"
    );
    assert_eq!(document.elements("").count(), 0);
    assert_tree(&document);
}

#[test]
fn doctype_identifiers_select_document_mode() {
    assert_eq!(html::parse("").quirks_mode, QuirksMode::Quirks);
    assert_eq!(
        html::parse("<!doctype html>").quirks_mode,
        QuirksMode::NoQuirks
    );
    assert_eq!(
        html::parse("<!doctype html PUBLIC '-//W3C//DTD HTML 4.01 Transitional//EN'>").quirks_mode,
        QuirksMode::Quirks
    );
    assert_eq!(
        html::parse("<!doctype html PUBLIC '-//W3C//DTD HTML 4.01 Transitional//EN' 'x'>")
            .quirks_mode,
        QuirksMode::LimitedQuirks
    );
    assert_eq!(
        html::parse("<!doctype html PUBLIC '-//W3C//DTD XHTML 1.0 Transitional//EN'>").quirks_mode,
        QuirksMode::LimitedQuirks
    );
    let malformed = html::parse("<!doctype html PUBLIC 'x' invalid>hello");
    assert_eq!(malformed.text(malformed.root), "hello");
    assert!(matches!(
        malformed.nodes[malformed.nodes[0].children[0].0].kind,
        NodeKind::Doctype { .. }
    ));
    let abrupt = html::parse("<!doctype html PUBLIC 'x' 'abrupt>hello");
    assert_eq!(abrupt.text(abrupt.root), "hello");
    assert_eq!(abrupt.quirks_mode, QuirksMode::Quirks);
}

#[test]
fn scripting_flag_controls_noscript_without_executing_scripts() {
    let source = "<!doctype html><head><noscript><meta name=x></noscript></head><body><noscript><b>fallback</b>&amp;</noscript><script>document.write('unexecuted')</script>";
    let disabled = html::parse(source);
    assert!(!disabled.scripting_enabled);
    assert_eq!(disabled.elements("meta").count(), 1);
    assert_eq!(disabled.elements("b").count(), 1);
    let enabled = html::parse_with_scripting(source, true);
    assert!(enabled.scripting_enabled);
    assert_eq!(enabled.elements("meta").count(), 0);
    assert_eq!(enabled.elements("b").count(), 0);
    let noscript: Vec<_> = enabled.elements("noscript").collect();
    assert_eq!(enabled.text(noscript[0]), "<meta name=x>");
    assert_eq!(enabled.text(noscript[1]), "<b>fallback</b>&amp;");
    assert!(
        enabled
            .serialize()
            .contains("<noscript><b>fallback</b>&amp;</noscript>")
    );
    assert_eq!(
        enabled.nodes.len(),
        html::parse_with_scripting(&enabled.serialize(), true)
            .nodes
            .len()
    );
    assert_tree(&enabled);
}
