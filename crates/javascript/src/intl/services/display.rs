use super::*;
use icu_experimental::displaynames::{DisplayNamesOptions, Fallback, Style, LanguageDisplay, multi::{RegionDisplayNames,ScriptDisplayNames,LocaleDisplayNamesFormatter}};
use icu_locale::{LocaleExpander,subtags::{Region,Script}};

mod index {include!("../cldr/display_index.rs");}
use index::NAMES;
const DATE_FIELDS:&[&str]=&["era","year","quarter","month","weekOfYear","weekday","day","dayPeriod","hour","minute","second","timeZoneName"];
struct DisplayNames {
    kind:String,tag:String,style:String,language_display:String,fallback:String,
    names:Names,
}
enum Names{Language(LocaleDisplayNamesFormatter), Region(RegionDisplayNames),Script(ScriptDisplayNames),Fields(Vec<String>)}
fn localized_data(l:&Locale)->Vec<String>{
    let mut selected=Vec::new();
    let mut add=|tag:&str|{
        if let Ok(i)=index::LOCALES.binary_search_by_key(&tag,|r|r.0){
            let (_,start,end)=index::LOCALES[i];
            let bytes=base64::Engine::decode(&base64::engine::general_purpose::STANDARD,&NAMES[start..end]).expect("checked-in CLDR data");
            let text=miniz_oxide::inflate::decompress_to_vec_zlib_with_limit(&bytes,131072).expect("bounded CLDR locale data");
            selected.push(String::from_utf8(text).expect("CLDR data is UTF-8"));
        }
    };
    let mut id=l.id.clone();add(&id.to_string());
    if id.script.is_none(){LocaleExpander::new_extended().maximize(&mut id);add(&id.to_string());}
    id.region=None;add(&id.to_string());add(id.language.as_str());
    selected
}
fn new_display(vm:&mut Vm,args:&[Value],p:ObjectId)->ResultValue{
    let (l,o)=requested(vm,args,false)?;
    let style=opt(vm,&o,"style",&["narrow","short","long"])?.unwrap_or_else(||"long".into());
    let kind=opt(vm,&o,"type",&["language","region","script","currency","calendar","dateTimeField"])?.ok_or_else(||JsError::type_error("Display name type required"))?;
    let fallback=opt(vm,&o,"fallback",&["code","none"])?.unwrap_or_else(||"code".into());
    let language_display=opt(vm,&o,"languageDisplay",&["dialect","standard"])?.unwrap_or_else(||"dialect".into());
    let tag=base(&l);
    let mut opts=DisplayNamesOptions::default();
    opts.style=Some(match style.as_str(){"short"=>Style::Short,"narrow"=>Style::Narrow,_=>Style::Long});
    opts.fallback=if fallback=="none"{Fallback::None}else{Fallback::Code};
    opts.language_display=if language_display=="dialect"{LanguageDisplay::Dialect}else{LanguageDisplay::Standard};
    let names=match kind.as_str(){
        "language"=>LocaleDisplayNamesFormatter::try_new(l.into(),opts).map(Names::Language),
        "region"=>RegionDisplayNames::try_new(l.into(),opts).map(Names::Region),
        "script"=>ScriptDisplayNames::try_new(l.into(),opts).map(Names::Script),
        _=>Ok(Names::Fields(localized_data(&l)))
    }.map_err(|_|range("Unsupported display name locale"))?;
    create(vm,p,DisplayNames{kind,tag,style,language_display,fallback,names})
}
fn display(vm:&mut Vm,this:Value,args:&[Value])->ResultValue{
    let f=slots::<DisplayNames>(vm,&this)?;
    let mut code=vm.to_string(&arg(args,0))?.to_string_lossy();
    let name=match &f.names{
        Names::Language(fmt)=>{
            let l=locale::parse(&code)?;
            if !l.extensions.is_empty(){return Err(range("Expected language identifier without extensions"))}
            code=l.id.to_string();
            let s=fmt.of(&l);(s!=code).then(||s.into_owned())
        }
        Names::Region(fmt)=>{let r:Region=code.parse().map_err(|_|range("Invalid region code"))?;code=r.to_string();fmt.of(r).map(str::to_owned)}
        Names::Script(fmt)=>{let s:Script=code.parse().map_err(|_|range("Invalid script code"))?;code=s.to_string();fmt.of(s).map(str::to_owned)}
        Names::Fields(tables)=>{
            let key=match f.kind.as_str(){
                "currency"=>{if code.len()!=3||!code.bytes().all(|b|b.is_ascii_alphabetic()){return Err(range("Invalid currency code"))}code.make_ascii_uppercase();code.clone()}
                "calendar"=>{
                    if !unicode_type(&code){return Err(range("Invalid calendar code"))}
                    code.make_ascii_lowercase();
                    if let Some(s)=locale::table(data::UNICODE_ALIASES,&format!("ca:{}",code)){code=s.to_owned()}
                    code.clone()
                }
                _=>{if !DATE_FIELDS.contains(&code.as_str()){return Err(range("Invalid date-time field"))}
                    format!("{}:{code}",f.style)}
            };
            tables.iter().find_map(|data|data.lines().find_map(|line|{let(k,v)=line.split_once('\t')?;(k==key).then_some(v)})).filter(|s|!s.is_empty()).map(str::to_owned)
        }
    };
    Ok(name.or_else(||(f.fallback=="code").then_some(code)).map(Value::from).unwrap_or_default())
}
pub(super) fn install(vm:&mut Vm,intl:Value){
    let(ctor,p)=class(vm,intl,"DisplayNames",2,false,new_display);supported(vm,&ctor);
    method(vm,p.clone(),"of",1,display);
    method(vm,p,"resolvedOptions",0,|vm,this,_|{
        let f=slots::<DisplayNames>(vm,&this)?;let o=vm.new_object();
        for(k,v)in[("locale",&f.tag),("style",&f.style),("type",&f.kind),("fallback",&f.fallback)]{field(vm,&o,k,v.as_str())?}
        if f.kind=="language"{field(vm,&o,"languageDisplay",f.language_display.as_str())?}Ok(o)
    });
}
