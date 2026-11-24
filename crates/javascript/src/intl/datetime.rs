//! Date and time formatting with ICU calendar/locale data and the IANA TZDB.
use super::*;
use crate::temporal::slots::{Kind, intl_fields};
use std::cell::RefCell;
use icu_calendar::{Date, Iso, types::RataDie};
use icu_datetime::{DateTimeFormatter, fieldsets::{builder::{DateFields, FieldSetBuilder},enums::CompositeDateTimeFieldSet}, input::{DateTime,Time}, options::{Length,YearStyle,TimePrecision,SubsecondDigits}};
use jiff::{Timestamp,tz::{Offset,TimeZone}};
use std::{collections::BTreeMap,fmt,time::{SystemTime,UNIX_EPOCH}};
use writeable::{Part,PartsWrite,Writeable};

#[derive(Default)]
struct Parts { spans:Vec<(&'static str,String)>, active:Option<&'static str> }
impl fmt::Write for Parts {
    fn write_str(&mut self,s:&str)->fmt::Result{
        if s.is_empty(){return Ok(())}
        let k=self.active.unwrap_or("literal");
        if let Some((key,value))=self.spans.last_mut() && *key==k {value.push_str(s)}
        else {self.spans.push((k,s.to_owned()))}
        Ok(())
    }
}
impl PartsWrite for Parts {
    type SubPartsWrite=Self;
    fn with_part(&mut self,p:Part,mut f:impl FnMut(&mut Self)->fmt::Result)->fmt::Result{
        let old=self.active;
        if p.category=="datetime" { self.active=Some(p.value); }
        let r=f(self);
        self.active=old;r
    }
}

type Fields=BTreeMap<&'static str,String>;
struct DateFormat {
    locale:String,calendar:String,numbering:String,zone:TimeZone,zone_name:String,hour_cycle:String,
    locale_preferences: Locale,
    use_defaults: bool,
    temporal_formats: RefCell<BTreeMap<Kind, (DateTimeFormatter<CompositeDateTimeFieldSet>, Fields)>>,
    fields:Fields,date_style:Option<String>,time_style:Option<String>,formatter:DateTimeFormatter<CompositeDateTimeFieldSet>,
}
const COMPONENTS:&[(&str,&[&str])]=&[
    ("weekday",&["narrow","short","long"]),("era",&["narrow","short","long"]),("year",&["numeric","2-digit"]),
    ("month",&["numeric","2-digit","narrow","short","long"]),("day",&["numeric","2-digit"]),
    ("dayPeriod",&["narrow","short","long"]),("hour",&["numeric","2-digit"]),("minute",&["numeric","2-digit"]),("second",&["numeric","2-digit"]),
];
const STYLES:&[&str]=&["full","long","medium","short"];
fn zone_name(value:Option<String>)->Result<(String,TimeZone),JsError>{
    let Some(s)=value else {
        let z=TimeZone::system();
        let name=z.iana_name().unwrap_or("UTC");
        return Ok((locale::table(data::ZONE_ALIASES,&name.to_ascii_lowercase()).unwrap_or(name).to_owned(),z))
    };
    if let Some(off)=s.strip_prefix(['+','-']) {
        let digits=off.replace(':',"");
        if !(off.len()==2 || off.len()==4 || off.len()==5&&off.as_bytes()[2]==b':') || !digits.bytes().all(|b|b.is_ascii_digit()) || ![2,4].contains(&digits.len()){return Err(range("Invalid UTC offset"))}
        let h:i32=digits[..2].parse().unwrap();let m:i32=digits.get(2..4).unwrap_or("0").parse().unwrap();
        if h>23||m>=60{return Err(range("Invalid UTC offset"))}
        let secs=(h*3600+m*60)*if s.starts_with('-'){-1}else{1};
        let offset=Offset::from_seconds(secs).map_err(|_|range("Invalid UTC offset"))?;
        return Ok((format!("{}{:02}:{:02}",if secs<0{'-'}else{'+'},h,m),TimeZone::fixed(offset)))
    }
    let n=locale::table(data::ZONE_ALIASES,&s.to_ascii_lowercase()).ok_or_else(||range("Unknown time zone"))?;
    let zone=TimeZone::get(n).map_err(|_|range("Unknown time zone"))?;
    Ok((n.to_owned(),zone))
}
fn build(vm:&mut Vm,args:&[Value],proto:ObjectId)->ResultValue {
    build_with(vm,args,proto,"DateTimeFormat")
}
pub(crate) fn format_date(vm:&mut Vm,args:&[Value],millis:f64,method:&str)->ResultValue {
    let f=build_with(vm,args,prototype(vm,"DateTimeFormat"),method)?;
    let fmt=slots::<DateFormat>(vm,&f)?;
    render(vm,fmt.format_local(&fmt.formatter,&fmt.fields,fmt.local_millis(millis as i64))?,false,None)
}
fn build_with(vm:&mut Vm,args:&[Value],proto:ObjectId,method:&str)->ResultValue {
    let locs=locale::list(vm,arg(args,0))?;
    let opts=options(vm,arg(args,1),true)?;
    opt(vm,&opts,"localeMatcher",&["lookup","best fit"])?;
    let mut l=resolve(&locs);
    let mut option_key=|vm:&mut Vm,name:&str,k:&str,allowed:&[&str]|->Result<Option<String>,JsError>{
        let v=opt(vm,&opts,name,&[])?;
        if let Some(s)=&v{
            if s.is_empty() || !s.split('-').all(|p|(3..=8).contains(&p.len()) && p.bytes().all(|x|x.is_ascii_alphanumeric())){return Err(range("Invalid Unicode option type"))}
            if allowed.contains(&s.as_str()){locale::set_key(&mut l,k,s)?}
        }
        Ok(v)
    };
    let cal_opt=option_key(vm,"calendar","ca",data::CALENDARS)?;
    let num_opt=option_key(vm,"numberingSystem","nu",data::NUMBERING)?;
    let hour12=get(vm,&opts,"hour12")?;
    let hc=opt(vm,&opts,"hourCycle",&["h11","h12","h23","h24"])?;
    let hc=if !matches!(hour12,Value::Undefined) {if hour12.is_truthy(){"h12".into()}else{"h23".into()}}else{hc.or_else(||locale::key(&l,"hc")).unwrap_or_else(||locale::regional(data::HOURS,&l).to_owned())};
    locale::set_key(&mut l,"hc",&hc)?;
    let zone=get(vm,&opts,"timeZone")?;
    let zone=if matches!(zone,Value::Undefined){None}else{Some(vm.to_string(&zone)?.to_string_lossy())};
    let (zone_name,zone)=zone_name(zone)?;
    let mut fields=Fields::new();
    for &(key,valid) in COMPONENTS {if let Some(s)=opt(vm,&opts,key,valid)?{fields.insert(key,s);}}
    let frac=get(vm,&opts,"fractionalSecondDigits")?;
    if !matches!(frac,Value::Undefined){
        let n=vm.to_number(&frac)?;
        if !(1.0..=3.0).contains(&n){return Err(range("Invalid fractionalSecondDigits"))}
        fields.insert("fractionalSecondDigits",(n.floor() as u32).to_string());
    }
    if let Some(s)=opt(vm,&opts,"timeZoneName",&["short","long","shortOffset","longOffset","shortGeneric","longGeneric"])?{fields.insert("timeZoneName",s);}
    opt(vm,&opts,"formatMatcher",&["basic","best fit"])?;
    let ds=opt(vm,&opts,"dateStyle",STYLES)?;let ts=opt(vm,&opts,"timeStyle",STYLES)?;
    if (method=="toLocaleTimeString" && ds.is_some()) || (method=="toLocaleDateString" && ts.is_some()) {
        return Err(JsError::type_error("Date style and time style must match the formatting operation"))
    }
    let date_requested=["year","month","day","weekday"].iter().any(|k|fields.contains_key(k));
    let time_requested=["dayPeriod","hour","minute","second","fractionalSecondDigits"].iter().any(|k|fields.contains_key(k));
    let needs_defaults = match method {
        "toLocaleDateString" => !date_requested,
        "toLocaleTimeString" => !time_requested,
        _ => !date_requested && !time_requested
    };
    let use_defaults = ds.is_none() && ts.is_none() && needs_defaults;
    if ds.is_some()||ts.is_some(){
        if !fields.is_empty(){return Err(JsError::type_error("Date and time styles cannot be combined with components"))}
        if let Some(s)=&ds {
            fields.insert("year","numeric".into());fields.insert("day","numeric".into());fields.insert("month",match s.as_str(){"full"|"long"=>"long","medium"=>"short",_=>"numeric"}.into());
            if s=="full"{fields.insert("weekday","long".into());}
        }
        if let Some(s)=&ts{
            fields.insert("hour","numeric".into());fields.insert("minute","2-digit".into());
            if s!="short"{fields.insert("second","2-digit".into());}
        }
    }else if needs_defaults{
        if method!="toLocaleTimeString" {for key in ["year","month","day"]{fields.insert(key,"numeric".into());}}
        if method=="toLocaleString"||method=="toLocaleTimeString" {for key in ["hour","minute","second"]{fields.insert(key,"numeric".into());}}
    }
    let formatter = make_formatter(&l, &fields)?;
    let calendar=locale::key(&l,"ca").filter(|s|data::CALENDARS.contains(&s.as_str())).unwrap_or_else(||locale::regional(data::CALENDAR_PREFS,&l).split_whitespace().next().unwrap_or("gregory").into());
    let numbering=locale::key(&l,"nu").filter(|s|data::NUMBERING.contains(&s.as_str())).unwrap_or_else(||locale::default_numbering(&l).into());
    let mut resolved:Locale=locale::lookup(&l).unwrap_or_else(||"en-US".into()).parse().unwrap();
    if cal_opt.is_none()&&locs.first().and_then(|l|locale::key(l,"ca")).as_deref()==Some(&calendar){locale::set_key(&mut resolved,"ca",&calendar)?;}
    if num_opt.is_none()&&locs.first().and_then(|l|locale::key(l,"nu")).as_deref()==Some(&numbering){locale::set_key(&mut resolved,"nu",&numbering)?;}
    if locs.first().and_then(|l|locale::key(l,"hc")).as_deref()==Some(&hc)&&matches!(hour12,Value::Undefined){locale::set_key(&mut resolved,"hc",&hc)?}
    vm.reserve_allocation(std::mem::size_of::<DateFormat>()+fields.len()*64)?;
    create(vm,proto,DateFormat{locale:resolved.to_string(),calendar,numbering,zone,zone_name,hour_cycle:hc,locale_preferences:l,use_defaults,temporal_formats:RefCell::new(BTreeMap::new()),fields,date_style:ds,time_style:ts,formatter})
}
fn make_formatter(l: &Locale, fields: &Fields) -> Result<DateTimeFormatter<CompositeDateTimeFieldSet>, JsError> {
    let mut b=FieldSetBuilder::new();
    let has=|k:&str|fields.contains_key(k);
    let (y,m,d,e)=(has("year")||has("era"),has("month"),has("day"),has("weekday"));
    b.date_fields=match (y,m,d,e){
        (false,false,false,false)=>None,(true,false,false,false)=>Some(DateFields::Y),(false,true,false,false)=>Some(DateFields::M),
        (true,true,false,false)=>Some(DateFields::YM),(false,false,true,false)=>Some(DateFields::D),(false,false,false,true)=>Some(DateFields::E),
        (false,true,true,false)=>Some(DateFields::MD),(false,false,true,true)=>Some(DateFields::DE),(false,true,true,true)=>Some(DateFields::MDE),
        (_,_,_,true)=>Some(DateFields::YMDE),_=>Some(DateFields::YMD)
    };
    b.length=Some(match fields.get("month").map(String::as_str).or_else(||fields.get("weekday").map(String::as_str)){
        Some("long"|"narrow")=>Length::Long,Some("short")=>Length::Medium,_=>Length::Short
    });
    if has("year")||has("era"){b.year_style=Some(if has("era"){YearStyle::WithEra}else{YearStyle::Full});}
    if has("hour")||has("minute")||has("second")||has("fractionalSecondDigits")||has("dayPeriod"){
        b.time_precision=Some(if let Some(f)=fields.get("fractionalSecondDigits"){TimePrecision::Subsecond(match f.as_str(){"1"=>SubsecondDigits::S1,"2"=>SubsecondDigits::S2,_=>SubsecondDigits::S3})}
        else if has("second"){TimePrecision::Second}else if has("minute"){TimePrecision::Minute}else{TimePrecision::Hour});
    }
    let set=b.build_composite_datetime().map_err(|_|range("Unsupported date and time field combination"))?;
    DateTimeFormatter::try_new(l.clone().into(),set).map_err(|_|range("Cannot format this locale"))
}
fn millis(vm:&mut Vm,v:&Value)->Result<i64,JsError>{
    let x=if matches!(v,Value::Undefined){SystemTime::now().duration_since(UNIX_EPOCH).map_or(0.,|s|s.as_millis() as f64)}else{vm.to_number(v)?};
    if !x.is_finite() || x.abs()>8.64e15 {return Err(range("Invalid time value"))} Ok(x.trunc() as i64)
}
impl DateFormat{
    fn local_millis(&self,ms:i64)->i64 {
        let ts=Timestamp::from_millisecond(ms).unwrap_or_else(|_|if ms<0{Timestamp::MIN}else{Timestamp::MAX});
        ms+i64::from(self.zone.to_offset(ts).seconds())*1000
    }

    fn parts(&self, vm: &mut Vm, input: &Value) -> Result<(Option<Kind>, Vec<(&'static str, String)>), JsError> {
        let Some((kind, calendar, ms)) = intl_fields(vm, input)? else {
            let local = self.local_millis(millis(vm, input)?);
            return Ok((None, self.format_local(&self.formatter, &self.fields, local)?));
        };
        let must_match = matches!(kind, Kind::PlainYearMonth | Kind::PlainMonthDay)
            || matches!(kind, Kind::PlainDate | Kind::PlainDateTime) && calendar != "iso8601";
        if must_match && calendar != self.calendar {
            return Err(range("Temporal and formatter calendars differ"));
        }

        if !self.temporal_formats.borrow().contains_key(&kind) {
            let date = matches!(kind, Kind::Instant | Kind::PlainDateTime | Kind::PlainDate);
            let time = matches!(kind, Kind::Instant | Kind::PlainDateTime | Kind::PlainTime);
            let year = date || kind == Kind::PlainYearMonth;
            let day = date || kind == Kind::PlainMonthDay;
            let month = year || day;
            let mut fields = self.fields.clone();
            if self.use_defaults {
                for (key, enable) in [("year", year), ("month", month), ("day", day),
                    ("hour", time), ("minute", time), ("second", time)] {
                    if enable { fields.insert(key, "numeric".into()); }
                }
            }
            fields.retain(|&key, _| match key {
                "year" | "era" => year,
                "month" => month,
                "day" => day,
                "weekday" => date,
                "timeZoneName" => kind == Kind::Instant,
                _ => time,
            });
            if fields.is_empty() {
                return Err(JsError::type_error("Formatting options do not overlap with Temporal fields"));
            }
            let formatter = make_formatter(&self.locale_preferences, &fields)?;
            vm.reserve_allocation(std::mem::size_of_val(&formatter))?;
            self.temporal_formats.borrow_mut().insert(kind, (formatter,fields));
        }
        let formats = self.temporal_formats.borrow();
        let local = if kind == Kind::Instant { self.local_millis(ms) } else { ms };
        Ok((Some(kind), self.format_local(&formats[&kind].0, &formats[&kind].1, local)?))
    }
    fn format_local(&self, formatter: &DateTimeFormatter<CompositeDateTimeFieldSet>, fields: &Fields, local: i64)->Result<Vec<(&'static str,String)>,JsError>{
        let day=local.div_euclid(86400000); let time=local.rem_euclid(86400000);
        let datetime=DateTime{
            date:Date::from_rata_die(RataDie::new(day+719163),Iso),
            time:Time::try_new((time/3600000) as u8,(time/60000%60) as u8,(time/1000%60) as u8,(time%1000) as u32*1_000_000).unwrap(),
        };
        let mut parts=Parts::default();
        formatter.format(&datetime).write_to_parts(&mut parts).map_err(|_|range("Date formatting failed"))?;
        // CLDR short patterns may pad numeric month/day even when ECMA-402 requests
        // a single digit. Remove padding only from numeric representations.
        let zero = data::DIGIT_ZERO.iter().find(|(_, name)| *name == self.numbering).map(|(s,_)|*s).unwrap_or("0");
        for (key, text) in &mut parts.spans {
            if matches!(*key, "month" | "day") && fields.get(key).is_some_and(|s|s=="numeric")
                && text.starts_with(zero) && text.len()>zero.len() {
                text.drain(..zero.len());
            }
        }
        Ok(parts.spans)
    }
    fn resolved(&self,vm:&mut Vm)->ResultValue{
        let o=vm.new_object();
        for (k,v) in [("locale",&self.locale),("calendar",&self.calendar),("numberingSystem",&self.numbering),("timeZone",&self.zone_name)]{field(vm,&o,k,v.as_str())?}
        if self.fields.contains_key("hour"){field(vm,&o,"hourCycle",self.hour_cycle.as_str())?;field(vm,&o,"hour12",matches!(self.hour_cycle.as_str(),"h11"|"h12"))?}
        if self.date_style.is_none() && self.time_style.is_none(){
            for &(k,_) in COMPONENTS {if let Some(v)=self.fields.get(k){field(vm,&o,k,v.as_str())?}}
            if let Some(v)=self.fields.get("fractionalSecondDigits"){field(vm,&o,"fractionalSecondDigits",v.parse::<usize>().unwrap())?}
            if let Some(v)=self.fields.get("timeZoneName"){field(vm,&o,"timeZoneName",v.as_str())?}
        }
        if let Some(s)=&self.date_style{field(vm,&o,"dateStyle",s.as_str())?}
        if let Some(s)=&self.time_style{field(vm,&o,"timeStyle",s.as_str())?}
        Ok(o)
    }
}
fn render(vm:&mut Vm,parts:Vec<(&str,String)>,as_parts:bool,source:Option<&str>)->ResultValue{
    if !as_parts{return Ok(parts.into_iter().map(|(_,s)|s).collect::<String>().into())}
    let mut arr=Vec::with_capacity(parts.len());
    for (k,v) in parts{
        let o=vm.new_object();field(vm,&o,"type",k)?;field(vm,&o,"value",v)?;
        if let Some(source)=source{field(vm,&o,"source",source)?} arr.push(o);
    }
    vm.try_new_array(&arr)
}
pub(super) fn install(vm:&mut Vm,intl:Value){
    let (ctor,proto)=class(vm,intl,"DateTimeFormat",0,true,build);
    supported(vm,&ctor);
    accessor(vm,&proto,"format",|vm,this,_|{
        let fmt=slots::<DateFormat>(vm,&this)?;
        if let Some(cached)=vm.host_slot(&this,0){return Ok(cached)}
        let f=vm.native("",1,false,Rc::new(move|vm,_,args|{
            let (_, parts) = fmt.parts(vm, &arg(args,0))?; render(vm, parts, false, None)
        }));
        vm.set_host_slot(this,0,f.clone())?; Ok(f)
    });
    method(vm,proto.clone(),"formatToParts",1,|vm,this,args|{
        let f=slots::<DateFormat>(vm,&this)?;
        let (_, parts) = f.parts(vm, &arg(args,0))?; render(vm, parts, true, None)
    });
    method(vm,proto.clone(),"resolvedOptions",0,|vm,this,_|slots::<DateFormat>(vm,&this)?.resolved(vm));
    for name in ["formatRange","formatRangeToParts"]{
        method(vm,proto.clone(),name,2,move|vm,this,args|{
            let f=slots::<DateFormat>(vm,&this)?;
            if matches!(arg(args,0),Value::Undefined)||matches!(arg(args,1),Value::Undefined){return Err(JsError::type_error("Both range endpoints are required"))}
            let (start_type, left) = f.parts(vm, &arg(args,0))?;
            let (end_type, right) = f.parts(vm, &arg(args,1))?;
            if start_type != end_type { return Err(JsError::type_error("Range endpoints have different Temporal types")); }
            let as_parts=name=="formatRangeToParts";
            if left==right{return render(vm,left,as_parts,Some("shared"))}
            if !as_parts{return Ok(format!("{} – {}",left.into_iter().map(|(_,s)|s).collect::<String>(),right.into_iter().map(|(_,s)|s).collect::<String>()).into())}
            let mut out=Vec::new();
            for (seq,source) in [(left,"startRange"),(vec![("literal"," – ".into())],"shared"),(right,"endRange")]{
                for (k,v) in seq{let o=vm.new_object();field(vm,&o,"type",k)?;field(vm,&o,"value",v)?;field(vm,&o,"source",source)?;out.push(o)}
            }
            vm.try_new_array(&out)
        });
    }
}
