use rust_browser_engine::{LoadedPage, script::ScriptRuntime};
use rust_browser_javascript::{ErrorKind, Value};

fn realm(source: &str) -> ScriptRuntime {
    ScriptRuntime::new(&LoadedPage::from_html(
        "https://example.test/forms?old=1",
        source,
    ))
    .unwrap()
}

fn js(runtime: &mut ScriptRuntime, source: &str, expected: impl Into<Value>) {
    assert_eq!(runtime.eval(source).unwrap(), expected.into(), "{source}");
}

// Each Blob fetch queues a zero-delay task. A busy host can exhaust a frame's
// timer budget after one callback. Preserve the per-request assertions below.
fn pump_blob_fetches(runtime: &mut ScriptRuntime, requests: usize) {
    for _ in 0..requests {
        runtime.pump();
        if runtime
            .next_deadline()
            .is_none_or(|next| next > std::time::Instant::now())
        {
            break;
        }
    }
}

#[test]
fn blob_fetch_captures_requests_and_enforces_document_scope() {
    let mut runtime = realm("<!doctype html>");
    let url = runtime
        .eval(
            r##"
            var url = URL.createObjectURL(new Blob(["XabcdefY"]).slice(1,7,"text/plain"));
            var held = new Request(url + "#held"), log = [], receipt = {};
            async function read(key, input, options) {
                try {
                    const response = await fetch(input, options);
                    receipt[key] = [response.url, response.type, response.status,
                        response.headers.get("content-type"), response.headers.get("content-length"),
                        await response.text()].join("|");
                    log.push(key);
                } catch (error) {
                    receipt[key] = error.name;
                }
            }
            read("query", url + "?test");
            read("head", url, {method:"HEAD"});
            const controller = new AbortController();
            read("cancelled", url, {signal:controller.signal});
            controller.abort();
            URL.revokeObjectURL(url + "#no-effect");
            read("alreadyStarted", url + "#part");
            Promise.resolve().then(function(){ log.push("microtask"); });
            url;
            "##,
        )
        .unwrap();
    let mut other = ScriptRuntime::new(&LoadedPage::from_html(
        "https://other.test/",
        "<!doctype html>",
    ))
    .unwrap();
    let address = serde_json::to_string(&url.as_string().unwrap().to_string_lossy()).unwrap();
    other
        .eval(&format!(
            "var outcome; fetch({address}).catch(function(error) {{ outcome = error.name; }});"
        ))
        .unwrap();
    other.pump();
    js(&mut other, "outcome", "TypeError");
    assert!(other.take_network_jobs().is_empty());
    runtime
        .eval(
            r#"
            URL.revokeObjectURL(url);
            read("fresh", url);
            read("held", held);
            "#,
        )
        .unwrap();
    pump_blob_fetches(&mut runtime, 6);
    js(
        &mut runtime,
        r##"
        var full = url + "|basic|200|text/plain|6|abcdef";
        held.url === url + "#held" && log[0] === "microtask" &&
            receipt.alreadyStarted === full && receipt.held === full &&
            receipt.query === "TypeError" && receipt.head === "TypeError" &&
            receipt.fresh === "TypeError" && receipt.cancelled === "AbortError"
        "##,
        true,
    );
    assert!(runtime.take_network_jobs().is_empty());
    assert!(runtime.diagnostics().is_empty());
}

#[test]
fn blob_fetch_ranges_use_offsets_into_the_existing_slice() {
    let mut runtime = realm("<!doctype html>");
    runtime
        .eval(
            r#"
            var url = URL.createObjectURL(new Blob(["XabcdefY"]).slice(1,7));
            var ranges = {};
            async function read(key, range) {
                try {
                    const response = await fetch(url, {headers:{Range:range}});
                    ranges[key] = [response.status, response.statusText,
                        response.headers.get("content-length"), response.headers.get("content-range"),
                        await response.text()].join("|");
                } catch (error) {
                    ranges[key] = error.name;
                }
            }
            read("pair", "bytes = 1 - 3");
            read("suffix", "bytes=-2");
            read("open", "bytes=4-");
            read("clamp", "bytes=4-200");
            read("multiple", "bytes=0-1,4-5");
            read("beyond", "bytes=6-");
            "#,
        )
        .unwrap();
    pump_blob_fetches(&mut runtime, 6);
    js(
        &mut runtime,
        r#"
        ranges.pair === "206|Partial Content|3|bytes 1-3/6|bcd" &&
            ranges.suffix === "206|Partial Content|2|bytes 4-5/6|ef" &&
            ranges.open === ranges.suffix && ranges.clamp === ranges.suffix &&
            ranges.multiple === "TypeError" && ranges.beyond === "TypeError"
        "#,
        true,
    );
}

#[test]
fn blob_snapshots_view_bytes_after_options_and_reads_independent_results() {
    let mut runtime = realm("<!doctype html>");
    js(
        &mut runtime,
        r#"
        var array = new Uint8Array([7,0,255,128,9]);
        var sub = new DataView(array.buffer,1,3);
        var opts = {};
        Object.defineProperty(opts,'type',{get:function(){ array[2]=254; return 'Application/X-Binary'; }});
        var blob = new Blob([sub,'\ud800',new Blob(['\n'])],opts);
        array[2] = 17;
        var bytes, buffer;
        blob.bytes().then(function(value){ bytes=value; });
        blob.slice(-6,-1,'APPLICATION/X-SLICE').arrayBuffer().then(function(value){ buffer=value; });
        blob.size + ':' + blob.type
        "#,
        "7:application/x-binary",
    );
    runtime.pump();
    js(
        &mut runtime,
        r#"
        var view = new Uint8Array(buffer);
        var ok = bytes instanceof Uint8Array && buffer instanceof ArrayBuffer &&
            String(Array.from(bytes))==='0,254,128,239,191,189,10' &&
            String(Array.from(view))==='254,128,239,191,189';
        bytes[1] = 77;
        blob.bytes().then(function(value){ bytes=value; });
        ok
        "#,
        true,
    );
    runtime.pump();
    js(
        &mut runtime,
        "bytes[1]===254 && blob.slice(4,1).size===0 && blob.slice().type===''",
        true,
    );
}

#[test]
fn file_inheritance_dictionary_conversion_and_utf8_decode() {
    let mut runtime = realm("<!doctype html>");
    js(
        &mut runtime,
        r#"
        class Special extends File {}
        var order = [];
        var options = {};
        Object.defineProperty(options,'lastModified',{get:function(){ order.push('lastModified'); return -2.9; }});
        Object.defineProperty(options,'type',{get:function(){ order.push('type'); return 'TEXT/PLAIN'; }});
        Object.defineProperty(options,'endings',{get:function(){ order.push('endings'); return 'transparent'; }});
        var file = new Special(['\ufeffcaf\u00e9', new Uint8Array([255,0])], 'path/\ud800', options);
        var text, invalid;
        file.text().then(function(value){ text=value; });
        Blob.prototype.text.call({}).catch(function(error){ invalid=error.name; });
        var info = [order.join(','),file.name,file.type,file.lastModified,file.size,
            file instanceof Special,file instanceof File,file instanceof Blob,
            file.slice() instanceof File,Object.getPrototypeOf(File)===Blob,
            Object.prototype.toString.call(file)];
        info.join('|')
        "#,
        "endings,type,lastModified|path/�|text/plain|-2|10|true|true|true|false|true|[object File]",
    );
    runtime.pump();
    js(
        &mut runtime,
        r#"text==='café\ufffd\u0000' && invalid==='TypeError' &&
            new Blob([], {type:'text/é'}).type==='' &&
            new Blob(['a\rb\nc\r\nd'], {endings:'transparent'}).size===8"#,
        true,
    );
    let native = if cfg!(windows) {
        "a\r\nb\r\nc\r\nd"
    } else {
        "a\nb\nc\nd"
    };
    js(
        &mut runtime,
        r#"new Blob(['a\rb\nc\r\nd'],{endings:'native'}).text().then(function(value){text=value;});undefined"#,
        Value::Undefined,
    );
    runtime.pump();
    js(&mut runtime, "text", native);
    assert_eq!(
        runtime.eval("new Blob(null)").unwrap_err().kind,
        ErrorKind::TypeError
    );
}

#[test]
fn file_reads_follow_microtasks_and_buffer_source_conversion_checks_backing() {
    let mut runtime = realm("<!doctype html>");
    js(
        &mut runtime,
        r#"
        var order = [];
        var detached = new ArrayBuffer(2);
        var view = new Uint8Array(detached);
        detached.transfer();
        var fixed = new Blob([view,'file']);
        var resizable = new ArrayBuffer(2,{maxByteLength:8});
        var refused;
        try { new Blob([resizable]); } catch (error) { refused=error instanceof TypeError; }
        fixed.text().then(function(text){ order.push(text); });
        Promise.resolve().then(function(){ order.push('first'); })
            .then(function(){ order.push('second'); });
        refused && fixed.size===4
        "#,
        true,
    );
    runtime.pump();
    js(&mut runtime, "order.join(',')", "first,second,file");
}

#[test]
fn form_data_has_live_ordered_iteration_and_keeps_file_wrappers_traced() {
    let mut runtime = realm("<!doctype html>");
    js(
        &mut runtime,
        r#"
        var fd = new FormData();
        fd.append('k','first');
        fd.append('k','second');
        fd.append('middle',undefined);
        var trace = [fd.getAll('k').join(',')];
        var it = fd.entries();
        trace.push(it.next().value.join(':'));
        fd.set('k','replacement');
        trace.push(it.next().value.join(':'));
        trace.push(it.next().done);
        fd.append('back',null);
        trace.push(it.next().value.join(':'));
        fd.forEach(function(value,name,self) {
            trace.push(name+':'+value);
            if (name==='k') { self.delete('middle'); self.append('later','\ud800'); }
        });
        trace.join('|')
        "#,
        "first,second|k:first|middle:undefined|true|back:null|k:replacement|back:null|later:�",
    );
    js(
        &mut runtime,
        r#"
        var original = new File(['body'],'first', {type:'Text/Plain',lastModified:102});
        var files = new FormData();
        files.append('v',original);
        files.append('v',original,'renamed');
        files.append('v',new Blob(['other']));
        original.mark=7;
        var alias = files.getAll('v')[1];
        var kept = files.values();
        var ok = files.get('v')===original && alias!==original &&
            alias.name==='renamed' && alias.lastModified===102 &&
            files.getAll('v')[2].name==='blob' && files.getAll('v')[2] instanceof File;
        alias.mark=9;
        files=null;
        original=null;
        alias=null;
        ok
        "#,
        true,
    );
    runtime.pump();
    js(
        &mut runtime,
        r#"kept.next().value.mark===7 && kept.next().value.mark===9 &&
            kept.next().value.name==='blob' && kept.next().done"#,
        true,
    );
}

#[test]
fn form_data_builds_successful_controls_and_separates_formdata_event_copy() {
    let mut runtime = realm(
        "<!doctype html><input form=f name=outside value=outside>\
         <form id=f><fieldset disabled><legend><input name=legend value=yes></legend>\
         <input name=hidden value=no></fieldset><input type=checkbox checked name=c>\
         <input type=checkbox name=off><input name=_charset_ type=hidden value=old>\
         <textarea id=edit name=t>initial</textarea>\
         <select name=s multiple><option selected value=one><option disabled selected value=two></select>\
         <input name=file type=file><button id=b name=button value=do>Submit</button></form>\
         <form><button id=wrong>Wrong</button></form>",
    );
    js(
        &mut runtime,
        r#"
        var f = document.getElementById('f');
        var b = document.getElementById('b');
        var edit = document.getElementById('edit');
        edit.value='a\r\nb';
        var seen, failure, eventData;
        f.onformdata = function(event) {
            seen = event instanceof FormDataEvent && event instanceof Event &&
                event.target===f && event.bubbles && !event.cancelable && event.isTrusted;
            eventData = event.formData;
            eventData.append('added','one');
            try { new FormData(f); } catch (error) { failure=error; }
            f.submit();
        };
        var result = new FormData(f,b);
        eventData.set('added','two');
        var empty = result.get('file');
        var wrong, bad;
        try { new FormData(f,document.getElementById('wrong')); } catch(error){ wrong=error; }
        try { new FormData(f,edit); } catch(error){ bad=error; }
        var event = new FormDataEvent('other', {formData:result,bubbles:true});
        seen && failure instanceof DOMException && failure.name==='InvalidStateError' &&
            wrong instanceof DOMException && wrong.name==='NotFoundError' && bad instanceof TypeError &&
            empty instanceof File && empty===eventData.get('file') &&
            empty.size===0 && empty.name==='' && empty.type==='application/octet-stream' &&
            result!==eventData && result.get('added')==='one' &&
            event.formData===result && event.bubbles && !event.isTrusted &&
            result.get('t')==='a\nb' && result.get('c')==='on' && result.get('s')==='one' &&
            result.get('_charset_')==='UTF-8' && result.get('button')==='do' &&
            Array.from(result.keys()).join(',')==='outside,legend,c,_charset_,t,s,file,button,added'
        "#,
        true,
    );
    assert!(runtime.take_form_submissions().is_empty());
    js(
        &mut runtime,
        r#"
        var detached = document.createElement('form');
        detached.innerHTML='<input name=x value=detached>';
        new FormData(detached).get('x')
        "#,
        "detached",
    );
}

#[test]
fn multipart_navigation_retains_exact_binary_and_escapes_parameters() {
    let mut runtime = realm(
        "<!doctype html><form id=f action=/upload method=post enctype=multipart/form-data>\
         <input name=empty type=file><button id=b name=go value=yes>Send</button></form>",
    );
    runtime
        .eval(
            r#"
            var f = document.getElementById('f');
            f.addEventListener('formdata',function(event) {
                event.formData.append('one"\ntwo','a\rb\nc');
                event.formData.append('bytes',
                    new File([new Uint8Array([0,255,13,10,128])], 'café"\r.bin',
                             {type:'Application/X-Binary'}));
            });
            f.requestSubmit(document.getElementById('b'));
            "#,
        )
        .unwrap();
    let forms = runtime.take_form_submissions();
    assert_eq!(forms.len(), 1);
    let post = &forms[0];
    assert_eq!(post.method, "POST");
    assert_eq!(post.url, "https://example.test/upload");
    let boundary = post
        .content_type
        .as_deref()
        .unwrap()
        .strip_prefix("multipart/form-data; boundary=")
        .unwrap();
    assert!((27..=70).contains(&boundary.len()));
    let prefix = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"empty\"; filename=\"\"\r\n\
         Content-Type: application/octet-stream\r\n\r\n\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"go\"\r\n\r\nyes\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"one%22%0D%0Atwo\"\r\n\r\na\r\nb\r\nc\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"bytes\"; filename=\"café%22%0D.bin\"\r\n\
         Content-Type: application/x-binary\r\n\r\n"
    );
    let mut expected = prefix.into_bytes();
    expected.extend_from_slice(&[0, 255, 13, 10, 128]);
    expected.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    assert_eq!(post.body.as_deref(), Some(expected.as_slice()));
    assert!(runtime.diagnostics().is_empty());
}

#[test]
fn native_url_encoding_uses_filename_and_rechecks_action_after_formdata() {
    let mut runtime =
        realm("<!doctype html><form id=f action=/before><input name=x value=2></form>");
    runtime
        .eval(
            r#"
            var f = document.getElementById('f');
            f.onformdata = function(event) {
                f.action='/after?discard=this';
                event.formData.append('q',new File(['unused'],'café\n b.txt'));
            };
            f.submit();
            "#,
        )
        .unwrap();
    let forms = runtime.take_form_submissions();
    assert_eq!(forms.len(), 1);
    assert_eq!(
        forms[0].url,
        "https://example.test/after?x=2&q=caf%C3%A9%0D%0A+b.txt"
    );
    assert!(forms[0].content_type.is_none() && forms[0].body.is_none());
    runtime
        .eval("f.onformdata=function(){f.remove();};f.submit();")
        .unwrap();
    assert!(runtime.take_form_submissions().is_empty());
}

#[test]
fn fetch_body_readers_round_trip_multipart_and_urlencoded_fields() {
    let mut runtime = realm("<!doctype html>");
    runtime
        .eval(
            r#"
            var fd = new FormData();
            fd.append('field','a\rb\nc');
            fd.append('field','\u{1f980}');
            fd.append('bin', new File([new Uint8Array([0,255,10,168])], 'café.bin'));
            var request = new Request('/upload', {method:'POST', body:fd});
            var duplicate = request.clone();
            fd.set('field','too late');
            fetch(duplicate);
            var multipart, payload, query;
            function recordMultipart(data) {
                multipart=data;
                return data.get('bin').bytes();
            }
            request.formData().then(recordMultipart).then(function(bytes){payload=bytes;});
            var headers = {'Content-Type':'application/x-www-form-urlencoded; charset=UTF-8'};
            new Response('k=one+k&k=%F0%9F%A6%80&odd=%FF', {headers:headers})
                .formData().then(function(data){query=data;});
            "#,
        )
        .unwrap();
    let requests = runtime.take_network_requests();
    assert_eq!(requests.len(), 1);
    let wire = &requests[0].1;
    let mime = wire
        .headers
        .iter()
        .find(|(name, _)| name == "content-type")
        .unwrap();
    assert!(mime.1.starts_with("multipart/form-data; boundary="));
    assert!(
        wire.body
            .as_ref()
            .unwrap()
            .windows(4)
            .any(|span| span == [0, 255, 10, 168])
    );
    runtime.pump();
    js(
        &mut runtime,
        r#"multipart.getAll('field').join('|')==='a\r\nb\r\nc|\u{1f980}' &&
            multipart.get('bin').name==='café.bin' &&
            multipart.get('bin').type==='application/octet-stream' &&
            String(Array.from(payload))==='0,255,10,168' &&
            query.getAll('k').join(',')==='one k,\u{1f980}' && query.get('odd')==='\ufffd'"#,
        true,
    );
}

#[test]
fn multipart_reader_keeps_near_boundary_bytes_and_rejects_truncation() {
    let mut runtime = realm("<!doctype html>");
    runtime
        .eval(
            r#"
            var headers = {'Content-Type':'multipart/form-data; boundary="test-boundary"'};
            var prefix = 'Preamble\r\n--test-boundary\r\n';
            var input = prefix + 'Content-Disposition: form-data; name="bytes"; filename="file.bin"\r\n\r\n' +
                'near\r\n--test-boundary-invalid\r\npayload\r\n--test-boundary\r\n' +
                'Content-Disposition: form-data; name="_charset_"\r\n\r\n\ufeffraw\r\n--test-boundary--\r\nEpilogue';
            var data, bytes, rejected;
            var good = new Response(input,{headers:headers});
            good.formData().then(function(result){
                data=result;
                return result.get('bytes').text();
            }).then(function(text){ bytes=text; });
            var bad = new Response(input.slice(0,input.lastIndexOf('--test-boundary--')), {headers:headers});
            bad.formData().catch(function(error){ rejected=error instanceof TypeError; });
            "#,
        )
        .unwrap();
    runtime.pump();
    js(
        &mut runtime,
        r#"bytes==='near\r\n--test-boundary-invalid\r\npayload' &&
            data.get('bytes').type==='text/plain' && data.get('_charset_')==='\ufeffraw' &&
            good.bodyUsed && bad.bodyUsed && rejected"#,
        true,
    );
}
