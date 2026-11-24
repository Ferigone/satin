use super::*;
use fixed_decimal::{Decimal, FloatPrecision};
use icu_experimental::duration::{Duration, DurationFormatter, DurationSign, ValidatedDurationFormatterOptions, options::*};
use icu_experimental::relativetime::{RelativeTimeFormatter, options::{Numeric, RelativeTimeFormatterOptions}};

/// Both services use the same ResolveLocale key and locale negotiation.
fn numbered_locale(vm:&mut Vm,args:&[Value])->Result<(Locale, String, String, Value),JsError>{
    let(mut l,o)=requested(vm,args,false)?;
    let opt=opt(vm,&o,"numberingSystem",&[])?;
    if opt.as_deref().is_some_and(|s|!unicode_type(s)){return Err(range("Invalid numbering system"))}
    let ext=locale::key(&l,"nu");let mut tag:Locale=base(&l).parse().unwrap();
    let number=opt.as_ref().or(ext.as_ref()).filter(|s|data::NUMBERING.contains(&s.as_str())).cloned().unwrap_or_else(||locale::default_numbering(&tag).to_string());
    if ext.as_ref()==Some(&number)&&opt.as_ref().is_none_or(|n|n==&number){locale::set_key(&mut tag,"nu",&number)?}
    locale::set_key(&mut l,"nu",&number)?;
    Ok((l,tag.to_string(),number,o))
}
#[derive(Default)]
struct TimeParts { fields:Vec<(&'static str,String,Option<&'static str>)>, kind:&'static str, unit:Option<&'static str> }
impl fmt::Write for TimeParts{
    fn write_str(&mut self,s:&str)->fmt::Result{
        if !s.is_empty(){
            let k=if self.kind.is_empty(){"literal"}else{self.kind};
            if let Some((kind,v,u))=self.fields.last_mut()&&*kind==k&&*u==self.unit{v.push_str(s)}
            else {self.fields.push((k,s.into(),self.unit))}
        }
        Ok(())
    }
}
impl PartsWrite for TimeParts{
    type SubPartsWrite=Self;
    fn with_part(&mut self,p:Part,mut f:impl FnMut(&mut Self)->fmt::Result)->fmt::Result{
        let old=(self.kind,self.unit);
        if p.category=="unit"{self.unit=Some(p.value)}
        else if matches!(p.value,"decimal"|"integer"|"fraction"|"group"|"minusSign"|"plusSign"|"unit"|"literal"){self.kind=p.value}
        let result=f(self);(self.kind,self.unit)=old;result
    }
}
impl TimeParts {
 fn result(self,vm:&mut Vm,parts:bool,relative_unit:Option<&str>)->ResultValue{
    if !parts{return Ok(self.fields.into_iter().map(|(_,v,_)|v).collect::<String>().into())}
    let mut arr=Vec::new();
    for(k,v,unit)in self.fields{
        let o=vm.new_object();field(vm,&o,"type",k)?;field(vm,&o,"value",v)?;
        if let Some(u)=unit.or(relative_unit.filter(|_|k!="literal")){field(vm,&o,"unit",u)?}
        arr.push(o)
    }vm.try_new_array(&arr)
 }
}
struct Relative{l:Locale,tag:String,number:String,numeric:String,style:String}
fn new_relative(vm:&mut Vm,args:&[Value],p:ObjectId)->ResultValue{
    let(l,tag,number,o)=numbered_locale(vm,args)?;
    let style=opt(vm,&o,"style",&["long","short","narrow"])?.unwrap_or_else(||"long".into());
    let numeric=opt(vm,&o,"numeric",&["always","auto"])?.unwrap_or_else(||"always".into());
    create(vm,p,Relative{l,tag,number,numeric,style})
}
fn relative(vm:&mut Vm,f:&Relative,args:&[Value],parts:bool)->ResultValue{
    let x=vm.to_number(&arg(args,0))?;
    let unit=vm.to_string(&arg(args,1))?.to_string_lossy();
    let unit=unit.strip_suffix('s').unwrap_or(&unit);
    let mut value=Decimal::try_from_f64(x,FloatPrecision::RoundTrip).map_err(|_|range("Invalid relative time value"))?;
    value.round(-3);value.trim_end();
    use RelativeTimeFormatter as F;
    let ctor=match(unit,f.style.as_str()){
        ("second", "long") => F::try_new_long_second,
        ("second", "short") => F::try_new_short_second,
        ("second", "narrow") => F::try_new_narrow_second,
        ("minute", "long") => F::try_new_long_minute,
        ("minute", "short") => F::try_new_short_minute,
        ("minute", "narrow") => F::try_new_narrow_minute,
        ("hour", "long") => F::try_new_long_hour,
        ("hour", "short") => F::try_new_short_hour,
        ("hour", "narrow") => F::try_new_narrow_hour,
        ("day", "long") => F::try_new_long_day,
        ("day", "short") => F::try_new_short_day,
        ("day", "narrow") => F::try_new_narrow_day,
        ("week", "long") => F::try_new_long_week,
        ("week", "short") => F::try_new_short_week,
        ("week", "narrow") => F::try_new_narrow_week,
        ("month", "long") => F::try_new_long_month,
        ("month", "short") => F::try_new_short_month,
        ("month", "narrow") => F::try_new_narrow_month,
        ("quarter", "long") => F::try_new_long_quarter,
        ("quarter", "short") => F::try_new_short_quarter,
        ("quarter", "narrow") => F::try_new_narrow_quarter,
        ("year", "long") => F::try_new_long_year,
        ("year", "short") => F::try_new_short_year,
        ("year", "narrow") => F::try_new_narrow_year,
        _=>return Err(range("Invalid relative time unit"))
    };
    let opts=RelativeTimeFormatterOptions::default().with_numeric(if f.numeric=="auto"{Numeric::Auto}else{Numeric::Always});
    let fmt=ctor(f.l.clone().into(),opts).map_err(|_|range("Unsupported relative time locale"))?;
    let mut out=TimeParts::default();
    fmt.format(value).write_to_parts(&mut out).map_err(|_|range("Relative time formatting failed"))?;
    out.result(vm,parts,Some(unit))
}
const UNITS:[&str;10]=["years","months","weeks","days","hours","minutes","seconds","milliseconds","microseconds","nanoseconds"];
struct DurationFormat{fmt:DurationFormatter,tag:String,number:String,style:String,fields:Vec<(String,String)>,frac:Option<u8>}
fn new_duration(vm:&mut Vm,args:&[Value],p:ObjectId)->ResultValue{
    let(l,tag,number,o)=numbered_locale(vm,args)?;
    let style=opt(vm,&o,"style",&["long","short","narrow","digital"])?.unwrap_or_else(||"short".into());
    let mut opts=DurationFormatterOptions::default();
    opts.base=match style.as_str(){"long"=>BaseStyle::Long,"narrow"=>BaseStyle::Narrow,"digital"=>BaseStyle::Digital,_=>BaseStyle::Short};
    let mut fields=Vec::new();
    let mut prev_numeric=false;
    for (i,name) in UNITS.iter().enumerate(){
        let valid=if i<4{&["long","short","narrow"][..]}else if i<7{&["long","short","narrow","numeric","2-digit"][..]}else{&["long","short","narrow","numeric"][..]};
        let v=opt(vm,&o,name,valid)?;
        let default_display=if v.is_some() || style=="digital" && (4..7).contains(&i) || prev_numeric && (5..7).contains(&i) {"always"}else{"auto"};
        let mut s=v.unwrap_or_else(||{
            if style=="digital"{if i<4{"short"}else{"numeric"}}
            else if prev_numeric{"numeric"}else{&style}
            .to_owned()
        });
        let implicit=if i>=7&&s=="numeric"{"auto"}else{default_display};
        let d=opt(vm,&o,&format!("{name}Display"),&["auto","always"])?.unwrap_or_else(||implicit.into());
        if prev_numeric && !matches!(s.as_str(),"numeric"|"2-digit"){return Err(range("A numeric duration field requires numeric smaller units"))}
        if prev_numeric&&(5..7).contains(&i){s="2-digit".into()}
        if i>=7&&s=="numeric"&&d=="always"{return Err(range("Fractional duration fields require automatic display"))}
        prev_numeric=matches!(s.as_str(),"numeric"|"2-digit");
        let disp=Some(if d=="always"{FieldDisplay::Always}else{FieldDisplay::Auto});
        macro_rules! assign {($field:ident, $vis:ident, $ty:ident $(, $value:literal => $variant:ident)* ) => {{
            opts.$field=Some(match s.as_str(){"long"=>$ty::Long,"narrow"=>$ty::Narrow, $($value=>$ty::$variant,)* _=>$ty::Short});
            opts.$vis=disp;
        }};}
        match i {
            0=>assign!(year,year_visibility,YearStyle),1=>assign!(month,month_visibility,MonthStyle),
            2=>assign!(week,week_visibility,WeekStyle),3=>assign!(day,day_visibility,DayStyle),
            4=>assign!(hour,hour_visibility,HourStyle,"numeric"=>Numeric,"2-digit"=>TwoDigit),
            5=>assign!(minute,minute_visibility,MinuteStyle,"numeric"=>Numeric,"2-digit"=>TwoDigit),
            6=>assign!(second,second_visibility,SecondStyle,"numeric"=>Numeric,"2-digit"=>TwoDigit),
            7=>assign!(millisecond,millisecond_visibility,MilliSecondStyle,"numeric"=>Numeric),
            8=>assign!(microsecond,microsecond_visibility,MicroSecondStyle,"numeric"=>Numeric),
            _=>assign!(nanosecond,nanosecond_visibility,NanoSecondStyle,"numeric"=>Numeric)
        }
        fields.push((s,d))
    }
    let v=get(vm,&o,"fractionalDigits")?;
    let frac=if matches!(v,Value::Undefined){None}else{
        let n=vm.to_number(&v)?;
        if !(0.0..=9.).contains(&n){return Err(range("Invalid duration fractionalDigits"))}Some(n.floor() as u8)
    };
    if let Some(n)=frac{opts.fractional_digits=FractionalDigits::Fixed(n)}
    let opts=ValidatedDurationFormatterOptions::validate(opts).map_err(|_|range("Invalid duration options"))?;
    let fmt=DurationFormatter::try_new(l.into(),opts).map_err(|_|range("Unsupported duration format"))?;
    create(vm,p,DurationFormat{fmt,tag,number,style,fields,frac})
}
fn duration_record(vm:&mut Vm,v:Value)->Result<Duration,JsError>{
    if v.as_object().is_none(){return Err(JsError::type_error("Expected duration object"))}
    let mut d=Duration::new();let mut sign=0;let mut any=false;
    for name in ["days","hours","microseconds","milliseconds","minutes","months","nanoseconds","seconds","weeks","years"]{
        let val=get(vm,&v,name)?;if matches!(val,Value::Undefined){continue}
        any=true;
        let n=vm.to_number(&val)?;
        if !n.is_finite()||n.trunc()!=n||n.abs()>=u64::MAX as f64{return Err(range("Duration field must be a finite integer"))}
        if n!=0.{
            let s=if n<0.{-1}else{1};
            if sign!=0&&s!=sign{return Err(range("Mixed duration signs"))} sign=s;
        }
        let place=match name{"days"=>&mut d.days,"hours"=>&mut d.hours,"microseconds"=>&mut d.microseconds,"milliseconds"=>&mut d.milliseconds,"minutes"=>&mut d.minutes,"months"=>&mut d.months,"nanoseconds"=>&mut d.nanoseconds,"seconds"=>&mut d.seconds,"weeks"=>&mut d.weeks,_=>&mut d.years};
        *place=n.abs() as u64;
    }
    if !any{return Err(JsError::type_error("Duration must specify at least one field"))}
    if d.years>=1<<32||d.months>=1<<32||d.weeks>=1<<32{return Err(range("Duration exceeds date field limits"))}
    let ns=(((u128::from(d.days)*24+u128::from(d.hours))*60+u128::from(d.minutes))*60+u128::from(d.seconds))*1_000_000_000+u128::from(d.milliseconds)*1_000_000+u128::from(d.microseconds)*1000+u128::from(d.nanoseconds);
    if ns>=((1u128<<53)*1_000_000_000){return Err(range("Duration exceeds time field limits"))}
    if sign<0{d.sign=DurationSign::Negative}Ok(d)
}
pub(super) fn install(vm:&mut Vm,intl:Value){
    let (ctor,p)=class(vm,intl.clone(),"RelativeTimeFormat",0,false,new_relative);supported(vm,&ctor);
    for name in ["format","formatToParts"]{method(vm,p.clone(),name,2,move|vm,this,args|relative(vm,slots::<Relative>(vm,&this)?.as_ref(),args,name=="formatToParts"));}
    method(vm,p,"resolvedOptions",0,|vm,this,_|{
        let f=slots::<Relative>(vm,&this)?;let o=vm.new_object();
        for(k,v)in[("locale",&f.tag),("style",&f.style),("numeric",&f.numeric),("numberingSystem",&f.number)]{field(vm,&o,k,v.as_str())?}Ok(o)
    });
    let(ctor,p)=class(vm,intl,"DurationFormat",0,false,new_duration);supported(vm,&ctor);
    for name in ["format","formatToParts"]{method(vm,p.clone(),name,1,move|vm,this,args|{
        let fmt=slots::<DurationFormat>(vm,&this)?;let duration=duration_record(vm,arg(args,0))?;let mut out=TimeParts::default();
        fmt.fmt.format(&duration).write_to_parts(&mut out).map_err(|_|range("Duration formatting failed"))?;
        out.result(vm,name=="formatToParts",None)
    });}
    method(vm,p,"resolvedOptions",0,|vm,this,_|{
        let f=slots::<DurationFormat>(vm,&this)?;let o=vm.new_object();field(vm,&o,"locale",f.tag.as_str())?;field(vm,&o,"numberingSystem",f.number.as_str())?;field(vm,&o,"style",f.style.as_str())?;
        for(i,(style,display))in f.fields.iter().enumerate(){field(vm,&o,UNITS[i],style.as_str())?;field(vm,&o,&format!("{}Display",UNITS[i]),display.as_str())?}
        if let Some(n)=f.frac{field(vm,&o,"fractionalDigits",usize::from(n))?}Ok(o)
    });
}
