use rust_browser_engine::{LoadedPage, script::ScriptRuntime};
use rust_browser_javascript::Value;

fn realm() -> ScriptRuntime {
    ScriptRuntime::new(&LoadedPage::from_html(
        "https://example.test/",
        "<!doctype html><title>Before</title><main id=parent><i id=before></i><b id=child>original</b><u id=after></u></main>",
    ))
    .unwrap()
}

fn expect(runtime: &mut ScriptRuntime, source: &str, result: Value) {
    assert_eq!(runtime.eval(source).unwrap(), result);
}

#[test]
fn options_conversion_and_old_values_follow_dom_and_web_idl() {
    let mut rt = realm();
    expect(
        &mut rt,
        r#"
var child = document.getElementById('child');
var text = child.firstChild;
var reads = [];
var observer = new MutationObserver(function(){ throw new Error('taken record delivered'); });
var options = {};
function option(key, trace, value) {
    Object.defineProperty(options, key, {get:function(){ reads.push(trace); return value; }});
}
option('subtree','subtree',true);
option('attributes','attributes',undefined);
option('attributeFilter','filter',['id','data-value']);
option('attributeOldValue','attributeOldValue',true);
option('characterDataOldValue','characterDataOldValue',true);
option('childList','childList',false);
option('characterData','characterData',undefined);
observer.observe(child, options);
child.setAttribute('data-value', 'one');
child.setAttribute('data-value', 'one');
child.removeAttribute('data-value');
child.removeAttribute('data-value');
child.setAttribute('data-nope', 'filtered');
child.id = 'new';
text.nodeValue = 'second';
text.data = 'third';
var saved = observer.takeRecords();
reads.join('|')
"#,
        "filter|attributeOldValue|attributes|characterData|characterDataOldValue|childList|subtree"
            .into(),
    );
    expect(&mut rt, "saved.map(function(r){return r.type+':'+r.attributeName+':'+r.oldValue;}).join('|')",
        "attributes:data-value:null|attributes:data-value:one|attributes:data-value:one|attributes:id:child|characterData:null:original|characterData:null:second".into());
    expect(
        &mut rt,
        r#"
var invalid = [{}, {attributes:false, attributeFilter:[]}, {childList:true, characterData:false, characterDataOldValue:true}, {attributes:true,attributeFilter:null}, false];
invalid.every(function(option){
    try { observer.observe(child, option); return false; }
    catch (e) { return e instanceof TypeError; }
}) && observer.takeRecords().length===0
"#,
        true.into(),
    );
    rt.pump();
    assert!(rt.diagnostics().is_empty(), "{:?}", rt.diagnostics());
}

#[test]
fn fragment_and_replace_records_hold_static_node_lists_with_sibling_identity() {
    let mut rt = realm();
    expect(
        &mut rt,
        r#"
var parent=document.getElementById('parent');
var before=document.getElementById('before');
var after=document.getElementById('after');
var child=document.getElementById('child');
var fragment=document.createDocumentFragment();
var a=document.createElement('strong'); a.id='a';
var b=document.createElement('small'); b.id='b';
fragment.append(a,b);
var observer=new MutationObserver(function(){});
observer.observe(parent,{childList:true});
observer.observe(fragment,{childList:true});
parent.replaceChild(fragment,child);
var records=observer.takeRecords(), from=records[0], to=records[1];
var replaced = records.length===2 && from.target===fragment &&
    from.previousSibling===null && from.nextSibling===null &&
    from.removedNodes.length===2 && from.removedNodes.item(0)===a && from.removedNodes[1]===b &&
    to.target===parent && to.previousSibling===before && to.nextSibling===after &&
    to.addedNodes instanceof NodeList && to.removedNodes instanceof NodeList &&
    to.addedNodes===to.addedNodes && to.removedNodes[0]===child &&
    to.addedNodes[0]===a && to.addedNodes[1]===b && to.attributeNamespace===null &&
    Object.prototype.toString.call(to)==='[object MutationRecord]' &&
    !Object.hasOwn(to,'type') && to instanceof MutationRecord;
parent.replaceChildren(a);
var changed=observer.takeRecords();
replaced && changed.length===1 && changed[0].addedNodes[0]===a &&
    changed[0].removedNodes.length===4 && changed[0].removedNodes[1]===a &&
    changed[0].previousSibling===null && changed[0].nextSibling===null
"#,
        true.into(),
    );
    rt.pump();
    expect(
        &mut rt,
        r#"
var list=to.addedNodes;
var ids=[];
list.forEach(function(n,i,owner){ if(owner===list){ids.push(i+':'+n.id);}});
ids.join('|')==='0:a|1:b' && b.parentNode===null && parent.firstChild===a &&
    list.length===2 && list.item(2)===null && list[2]===undefined
"#,
        true.into(),
    );
}

#[test]
fn observers_run_in_creation_order_on_the_fifo_and_allow_reentry() {
    let mut rt = realm();
    rt.eval(r#"
var trace=[];
var firstTarget=document.createElement('p'), secondTarget=document.createElement('p');
var aRound=0;
var first=new MutationObserver(function(recs, self) {
  trace.push(this===first && self===first && Array.isArray(recs) ? 'A:'+recs.length : 'wrong receiver');
  if (++aRound===1) {
    firstTarget.id='repeat';
    secondTarget.id='during';
  }
});
var second=new MutationObserver(function(recs) {
  trace.push('B:'+recs.length);
  second.disconnect();
});
first.observe(firstTarget,{attributes:true});
second.observe(secondTarget,{attributes:true});
Promise.resolve().then(function(){trace.push('promise-before');});
secondTarget.id='first-record';
queueMicrotask(function(){ trace.push('promise-after'); });
firstTarget.id='second-record';
"#).unwrap();
    expect(&mut rt, "trace.length", 0.into());
    rt.pump();
    expect(
        &mut rt,
        "trace.join('|')",
        "promise-before|A:1|B:2|promise-after|A:1".into(),
    );
    assert!(rt.diagnostics().is_empty());
}

#[test]
fn removed_subtrees_survive_take_records_and_clear_their_source_registration() {
    let mut rt = realm();
    expect(
        &mut rt,
        r#"
var parent=document.getElementById('parent'), child=document.getElementById('child');
var nested=child.appendChild(document.createElement('b'));
var observer=new MutationObserver(function(){});
observer.observe(parent,{attributes:true,subtree:true,attributeOldValue:true});
observer.observe(child,{childList:true,subtree:true});
child.remove();
nested.remove();
nested.id='detached';
var saves=observer.takeRecords();
observer.observe(child,{childList:true,subtree:true});
nested.id='still-from-parent';
var afterReplace=observer.takeRecords();
saves.length===2 && saves[0].target===child && saves[0].removedNodes[0]===nested &&
    saves[1].target===nested && saves[1].oldValue===null &&
    afterReplace.length===1 && afterReplace[0].oldValue==='detached'
"#,
        true.into(),
    );
    rt.pump();
    expect(
        &mut rt,
        r#"
nested.id='after-checkpoint';
observer.takeRecords().length===0 && saves[0].removedNodes[0]===nested &&
    nested.parentNode===null && child.parentNode===null
"#,
        true.into(),
    );
}

#[test]
fn registered_callbacks_and_user_retained_records_survive_collection() {
    let mut rt = realm();
    rt.eval(
        r#"
var saved, hits=0;
var parent=document.getElementById('parent');
(function() {
    var local=new MutationObserver(function(records, self) {
        hits++; saved=records[0]; self.disconnect();
    });
    local.observe(parent, {childList:true});
})();
"#,
    )
    .unwrap();
    rt.pump();
    rt.eval("parent.firstChild.remove();").unwrap();
    rt.pump();
    rt.eval("parent.replaceChildren();").unwrap();
    rt.pump();
    expect(
        &mut rt,
        r#"
hits===1 && saved.removedNodes[0].id==='before' && saved.removedNodes[0].parentNode===null &&
    saved.target===parent && saved.nextSibling.id==='child' && saved.nextSibling.parentNode===null
"#,
        true.into(),
    );
}

#[test]
fn record_budget_preserves_the_prefix_and_recovers_after_notification() {
    let mut rt = realm();
    expect(
        &mut rt,
        r#"
var size=0;
var target=document.getElementById('parent');
var observer=new MutationObserver(function(records){size+=records.length;});
observer.observe(target,{attributes:true,attributeOldValue:true});
for (var i=0;i<4200;i++) {target.setAttribute('data-value',String(i));}
var retained=observer.takeRecords();
retained.length===4096 && retained[0].oldValue===null &&
    retained[4095].oldValue==='4094' && target.getAttribute('data-value')==='4199'
"#,
        true.into(),
    );
    rt.pump();
    assert_eq!(rt.diagnostics().len(), 1);
    assert!(rt.diagnostics()[0].contains("pending record budget exceeded"));
    expect(&mut rt, "target.id='after'; size", 0.into());
    rt.pump();
    expect(&mut rt, "size", 1.into());
}

#[test]
fn attribute_filter_excludes_namespaced_attributes() {
    let mut rt = ScriptRuntime::new(&LoadedPage::from_html(
        "https://example.test/",
        "<!doctype html><svg><use id=element xlink:href='#old'></use></svg>",
    ))
    .unwrap();
    expect(
        &mut rt,
        r#"
var element=document.getElementById('element');
var all=new MutationObserver(function(){});
var filtered=new MutationObserver(function(){});
all.observe(element,{attributeOldValue:true});
filtered.observe(element,{attributeFilter:['href']});
element.setAttribute('xlink:href','#new');
element.setAttribute('href','#plain');
var records=all.takeRecords(), matching=filtered.takeRecords();
records.length===2 && records[0].attributeName==='href' &&
  records[0].attributeNamespace==='http://www.w3.org/1999/xlink' &&
  records[0].oldValue==='#old' && records[1].attributeNamespace===null &&
  matching.length===1 && matching[0].attributeNamespace===null
"#,
        true.into(),
    );
}
