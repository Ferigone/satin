use rust_browser_engine::{LoadedPage, script::ScriptRuntime};

#[test]
fn stylesheet_loader_can_reuse_an_existing_link_and_resolve_its_url() {
    let page = LoadedPage::from_html(
        "https://example.test/results/index.html",
        r#"
        <base href="/assets/"><link id="theme" rel="stylesheet" href="theme.css">
        <div id="result"></div>
    "#,
    );
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    assert_eq!(
        runtime
            .eval(
                r#"
        const links=document.getElementsByTagName('link');
        function ensure(href) {
          for (let i=0;i<links.length;i++) {
            if(links[i].href===href && links[i].rel==='stylesheet')return links[i];
          }
          const link=document.createElement('link');link.rel='stylesheet';link.href=href;
          document.head.appendChild(link);return link;
        }
        const original=document.getElementById('theme');
        const reused=ensure('https://example.test/assets/theme.css')===original;
        const second=ensure('https://example.test/assets/article.css');
        [reused,links.length,second.getAttribute('rel'),original.getAttribute('href'),
          original instanceof HTMLLinkElement].join('|')
    "#
            )
            .unwrap(),
        "true|2|stylesheet|theme.css|true".into()
    );
}

#[test]
fn changing_a_loaded_links_relation_restyles_the_document() {
    use std::sync::Arc;
    let mut page = LoadedPage::from_html(
        "https://example.test/",
        r#"
        <style>#card{width:30px}</style><link id=theme href="theme.css" rel="stylesheet">
        <div id=card></div>
    "#,
    );
    let source: Arc<str> = "#card{width:90px;background-color:green}".into();
    let link = page.document.html_elements("link").next().unwrap();
    page.linked_stylesheets.insert(link, source.clone());
    Arc::make_mut(&mut page.stylesheets).push(source);
    let mut runtime = ScriptRuntime::new(&page).unwrap();
    assert_eq!(
        runtime
            .eval(
                r#"
      const link=document.getElementById('theme'),card=document.getElementById('card');
      const width=()=>card.getBoundingClientRect().width;
      let active=width();link.rel='preload';let inactive=width();
      link.setAttribute('rel','bookmark STYLESHEET');
      [active,inactive,width()].join(',')
    "#
            )
            .unwrap(),
        "90,30,90".into()
    );
}
