use rust_browser_engine::{LoadedPage, script::ScriptRuntime};
use rust_browser_javascript::Value;

fn check(html: &str, source: &str) {
    let page = LoadedPage::from_html("https://example.test/walker", html);
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    assert_eq!(runtime.eval(source).unwrap(), Value::Bool(true));
}

#[test]
fn forward_walker_can_be_reused_on_detached_template_contents() {
    check(
        "<template id=t><i>A<!--marker--><b>B</b></i>tail</template>",
        r#"
        const fragment = document.getElementById('t').content;
        const walker = document.createTreeWalker(document, 129);
        walker.currentNode = fragment;
        const names = [];
        for (let next; (next = walker.nextNode()) !== null;) names.push(next.nodeName);
        const last = walker.currentNode;
        const texts = document.createTreeWalker(fragment, NodeFilter.SHOW_TEXT);
        const values = [];
        while (texts.nextNode()) values.push(texts.currentNode.data);
        walker.currentNode = document;
        walker instanceof TreeWalker && walker.root === document &&
            walker.currentNode === document && walker.filter === null && walker.whatToShow === 129 &&
            names.join(',') === 'I,#comment,B' && last.nodeName === 'B' &&
            values.join(',') === 'A,B,tail' &&
            document.createTreeWalker(fragment, undefined).whatToShow === NodeFilter.SHOW_ALL;
        "#,
    );
}

#[test]
fn filter_sees_live_children_skips_containers_and_prunes_rejected_subtrees() {
    check(
        "<main id=r><section id=s><b id=b></b></section><aside id=a><b id=x></b></aside><em id=e></em></main><b id=outside></b>",
        r#"
        const root = document.getElementById('r'), visited = [], accepted = [];
        const filter = { acceptNode(element) {
            if (this !== filter) throw 'wrong this';
            visited.push(element.id);
            if (element.id === 's') {
                const added = document.createElement('i');
                added.id = 'new';
                element.appendChild(added);
                return NodeFilter.FILTER_SKIP;
            }
            return element.id === 'a' ? NodeFilter.FILTER_REJECT : NodeFilter.FILTER_ACCEPT;
        }};
        const walker = document.createTreeWalker(root, NodeFilter.SHOW_ELEMENT, filter);
        while (walker.nextNode()) accepted.push(walker.currentNode.id);
        const last = walker.currentNode;
        const after = document.createElement('i');
        after.id = 'after';
        root.appendChild(after);
        walker.root === root && walker.filter === filter && last.id === 'e' &&
            accepted.join(',') === 'b,new,e' && visited.join(',') === 's,b,new,a,e' &&
            walker.nextNode() === after && walker.nextNode() === null && walker.currentNode === after;
        "#,
    );
}

#[test]
fn filter_errors_restore_the_walker_and_masked_nodes_do_not_call_author_code() {
    check(
        "<div id=root>text<i></i><b></b></div>",
        r#"
        const root = document.getElementById('root');
        let calls = 0, recursive = false, threw = false;
        const walker = document.createTreeWalker(root, 1, function(node){
            'use strict';
            if (this !== undefined || node.nodeType !== 1) throw 'wrong callback';
            calls++;
            if (calls === 1) {
                try { walker.nextNode(); } catch(e) { recursive = e.name === 'InvalidStateError'; }
                throw 'stop';
            }
            return 1;
        });
        try { walker.nextNode(); } catch(e) { threw = e === 'stop'; }
        recursive && threw && calls === 1 && walker.currentNode === root &&
            walker.nextNode().nodeName === 'I' && calls === 2;
        "#,
    );
}
