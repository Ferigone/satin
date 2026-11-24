use super::*;
use icu_locale::{LocaleCanonicalizer,LocaleDirectionality,LocaleExpander, extensions::unicode::{Key,Value as Keyword},subtags::{Variant,Variants}};

pub(super) fn parse(tag: &str) -> Result<Locale,JsError> {
    // Unicode locale identifiers exclude private-use-only and grandfathered tags.
    let lang=tag.split('-').next().unwrap_or("");
    if !(lang.len()==2 || lang.len()==3 || (5..=8).contains(&lang.len())) || tag.contains('_') {
        return Err(range("Invalid Unicode locale identifier"))
    }
    let mut seen=0u64;
    let mut variants=Vec::new();
    let mut extension=false;
    for part in tag.split('-').skip(1) {
        if part.len()==1 {
            extension=true;
            let c=part.as_bytes()[0].to_ascii_lowercase();
            if c==b'x' { break }
            let bit=if c.is_ascii_digit(){c-b'0'}else if c.is_ascii_lowercase(){10+c-b'a'}else{return Err(range("Invalid locale extension"))};
            if seen&(1<<bit)!=0{return Err(range("Duplicate locale extension"))}
            seen|=1<<bit;
        } else if !extension && (part.len()>=5 || part.len()==4 && part.as_bytes()[0].is_ascii_digit()) {
            let part=part.to_ascii_lowercase();
            if variants.contains(&part){return Err(range("Duplicate locale variant"))}
            variants.push(part);
        }
    }
    let mut locale:Locale=tag.parse().map_err(|_|range("Invalid Unicode locale identifier"))?;
    canonical(&mut locale);
    Ok(locale)
}
fn canonical(locale:&mut Locale) {
    LocaleCanonicalizer::new_extended().canonicalize(locale);
}
pub(super) fn list(vm:&mut Vm,value:Value)->Result<Vec<Locale>,JsError>{
    if matches!(value,Value::Undefined){return Ok(Vec::new())}
    if let Some(loc)=vm.host_data::<Locale>(&value){return Ok(vec![(*loc).clone()])}
    if let Value::String(s)=&value{return Ok(vec![parse(&s.to_string_lossy())?])}
    let obj=vm.coerce_object(value)?;
    let n=vm.get_property(obj.clone(),"length")?;
    let n=vm.to_number(&n)?.floor().max(0.);
    if n>vm.limits.max_array_length as f64{return Err(JsError::new(ErrorKind::ExecutionLimit,"Locale list is too long"))}
    let mut locales=Vec::new();
    for i in 0..n as usize{
        vm.charge_work(1)?;
        let key=i.to_string();
        if !vm.has(&obj,&key.as_str().into())?{continue}
        let v=vm.get_property(obj.clone(),&key)?;
        let l=locale_arg(vm,v)?;
        if !locales.contains(&l){locales.push(l)}
    }
    Ok(locales)
}
fn locale_arg(vm:&mut Vm,v:Value)->Result<Locale,JsError>{
    if let Some(l)=vm.host_data::<Locale>(&v){return Ok((*l).clone())}
    if !matches!(v, Value::String(_)|Value::Object(_)){return Err(JsError::type_error("Expected a locale string or object"))}
    parse(&vm.to_string(&v)?.to_string_lossy())
}
pub(super) fn lookup(locale:&Locale)->Option<String>{
    let s=locale.id.to_string();
    let mut tag=s.as_str();
    loop {
        if data::LOCALES.binary_search(&tag).is_ok(){return Some(tag.to_owned())}
        tag=tag.rsplit_once('-')?.0
    }
}
pub(super) fn key(l:&Locale,k:&str)->Option<String>{
    let value=l.extensions.unicode.keywords.get(&k.parse::<Key>().unwrap())?;
    Some(value.to_string())
}
pub(super) fn set_key(l:&mut Locale,k:&str,value:&str)->Result<(),JsError>{
    let v=if value=="true" {Keyword::default()}else {value.parse().map_err(|_|range("Invalid Unicode extension type"))?};
    l.extensions.unicode.keywords.set(k.parse().unwrap(),v);
    Ok(())
}
pub(super) fn table<'a>(rows:&'a [(&str,&str)], key:&str)->Option<&'a str>{
    rows.binary_search_by(|(k,_)|k.cmp(&key)).ok().map(|i|rows[i].1)
}
fn region_for(l:&Locale)->String {
    if let Some(r)=key(l,"rg").and_then(|s|s.strip_suffix("zzzz").map(str::to_uppercase)){return r}
    if let Some(region)=&l.id.region {return region.to_string()}
    if let Some(subdivision)=key(l,"sd") {
        let region=if subdivision.as_bytes().first().is_some_and(u8::is_ascii_digit){3}else{2};
        if let Some(r)=subdivision.get(..region){return r.to_ascii_uppercase()}
    }
    let mut id=l.id.clone();
    LocaleExpander::new_extended().maximize(&mut id);
    id.region.map_or_else(||"001".into(),|r|r.to_string())
}
pub(super) fn regional<'a>(table_data:&'a [(&str,&str)], l:&Locale)->&'a str{
    table(table_data,&region_for(l)).or_else(||table(table_data,"001")).unwrap_or("")
}
fn day_string(v:&str)->&str{
    match v{"0"|"7"=>"sun","1"=>"mon","2"=>"tue","3"=>"wed","4"=>"thu","5"=>"fri","6"=>"sat",s=>s}
}
fn day_no(s:&str)->Option<usize>{
    ["mon","tue","wed","thu","fri","sat","sun"].iter().position(|p|*p==s).map(|n|n+1)
}
fn new_locale(vm:&mut Vm,args:&[Value],proto:ObjectId)->ResultValue {
    let mut l=locale_arg(vm,arg(args,0))?;
    let opts=options(vm,arg(args,1),true)?;
    if let Some(s)=opt(vm,&opts,"language",&[])?{
        if !(s.len()==2||s.len()==3||(5..=8).contains(&s.len())){return Err(range("Invalid language subtag"))}
        l.id.language=s.parse().map_err(|_|range("Invalid language subtag"))?;
    }
    if let Some(s)=opt(vm,&opts,"script",&[])?{l.id.script=Some(s.parse().map_err(|_|range("Invalid script subtag"))?)}
    if let Some(s)=opt(vm,&opts,"region",&[])?{l.id.region=Some(s.parse().map_err(|_|range("Invalid region subtag"))?)}
    if let Some(s)=opt(vm,&opts,"variants",&[])?{
        let mut variants=Variants::new();
        for part in s.split('-') {
            let v:Variant=part.parse().map_err(|_|range("Invalid variant subtag"))?;
            if !variants.push(v){return Err(range("Duplicate variant"))}
        }
        l.id.variants=variants;
    }
    canonical(&mut l);
    for (name,k,valid) in [("calendar","ca",&[][..]),("collation","co",&[][..]),("firstDayOfWeek","fw",&[][..]),("hourCycle","hc",&["h11","h12","h23","h24"][..]),("caseFirst","kf",&["upper","lower","false"][..])] {
        if let Some(mut s)=opt(vm,&opts,name,valid)?{
            if k=="fw" {
                s=day_string(&s).into();
                if day_no(&s).is_none(){return Err(range("Invalid first day of week"))}
            }
            if s.is_empty(){return Err(range("Empty extension type"))}
            set_key(&mut l,k,&s)?;
        }
    }
    let n=get(vm,&opts,"numeric")?;
    if !matches!(n,Value::Undefined){set_key(&mut l,"kn",if n.is_truthy(){"true"}else{"false"})?}
    if let Some(s)=opt(vm,&opts,"numberingSystem",&[])?{
        if s.is_empty(){return Err(range("Empty numbering system"))}set_key(&mut l,"nu",&s)?;
    }
    canonical(&mut l);
    vm.reserve_allocation(l.to_string().len()*2)?;
    create(vm,proto,l)
}
pub(super) fn install(vm:&mut Vm,intl:Value){
    let (_,proto)=class(vm,intl,"Locale",1,false,new_locale);
    for (name,k) in [("calendar","ca"),("collation","co"),("firstDayOfWeek","fw"),("hourCycle","hc"),("caseFirst","kf"),("numberingSystem","nu")]{
        accessor(vm,&proto,name,move|vm,this,_|{
            let l=slots::<Locale>(vm,&this)?;Ok(key(&l,k).map(Value::from).unwrap_or_default())
        });
    }
    for name in ["language","script","region","variants","numeric","baseName"]{
        accessor(vm,&proto,name,move|vm,this,_|{
            let l=slots::<Locale>(vm,&this)?;
            let s=match name{
                "language"=>Some(l.id.language.to_string()),"script"=>l.id.script.map(|v|v.to_string()),"region"=>l.id.region.map(|v|v.to_string()),
                "baseName"=>Some(l.id.to_string()),
                "numeric"=>return Ok(key(&l,"kn").is_some_and(|s|s.is_empty()||s=="true").into()),
                _=>(!l.id.variants.is_empty()).then(||l.id.variants.iter().map(ToString::to_string).collect::<Vec<_>>().join("-"))
            };Ok(s.map(Value::from).unwrap_or_default())
        });
    }
    method(vm,proto.clone(),"toString",0,|vm,this,_|Ok(slots::<Locale>(vm,&this)?.to_string().into()));
    for name in ["maximize","minimize"]{
        method(vm,proto.clone(),name,0,move|vm,this,_|{
            let mut l=(*slots::<Locale>(vm,&this)?).clone();
            let exp=LocaleExpander::new_extended();
            if name=="maximize"{exp.maximize(&mut l.id);}else{exp.minimize(&mut l.id);}
            create(vm,prototype(vm,"Locale"),l)
        });
    }
    for name in ["getCalendars","getCollations","getHourCycles","getNumberingSystems","getTimeZones","getTextInfo","getWeekInfo"]{
        method(vm,proto.clone(),name,0,move|vm,this,_|{
            let loc=slots::<Locale>(vm,&this)?;
            let l=loc.as_ref();
            match name{
                "getTextInfo"=>{
                    let o=vm.new_object();
                    field(vm,&o,"direction",if LocaleDirectionality::new_extended().is_right_to_left(&l.id){"rtl"}else{"ltr"})?;
                    Ok(o)
                }
                "getWeekInfo"=>{
                    let first=key(l,"fw").and_then(|s|day_no(&s)).or_else(||day_no(regional(data::FIRST_DAY,l))).unwrap_or(1);
                    let start=day_no(regional(data::WEEKEND_START,l)).unwrap_or(6);let end=day_no(regional(data::WEEKEND_END,l)).unwrap_or(7);
                    let weekend:Vec<Value>=(1usize..=7).filter(|&d| if end>=start {(start..=end).contains(&d)}else{d>=start||d<=end}).map(Value::from).collect();
                    let o=vm.new_object();field(vm,&o,"firstDay",first)?;
                    let arr=vm.try_new_array(&weekend)?;field(vm,&o,"weekend",arr)?;
                    Ok(o)
                }
                "getTimeZones"=>{
                    let Some(region)=l.id.region else {return Ok(Value::Undefined)};
                    let zones=table(data::REGION_ZONES,&region.to_string()).unwrap_or("");
                    strings(vm,zones.split_whitespace())
                }
                _=>{
                    let (k, fallback)=match name{
                        "getCalendars"=>("ca",regional(data::CALENDAR_PREFS,l)),
                        "getHourCycles"=>("hc",regional(data::HOURS,l)),
                        "getCollations"=>("co",""),
                        _=>("nu",default_numbering(l))
                    };
                    if let Some(s)=key(l,k){strings(vm,[s])}else{strings(vm,fallback.split_whitespace())}
                }
            }
        });
    }
}
pub(super) fn default_numbering(l:&Locale)->&'static str {
    // Obtain the locale default from ICU's decimal number data.
    let Ok(fmt)=icu_decimal::DecimalFormatter::try_new(l.clone().into(),Default::default()) else {return "latn"};
    let zero=fmt.format(&fixed_decimal::Decimal::from(0)).to_string();
    data::DIGIT_ZERO.iter().find_map(|(c,n)|(*c==zero).then_some(*n)).unwrap_or("latn")
}
