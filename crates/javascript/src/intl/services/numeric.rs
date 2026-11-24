use super::*;
mod patterns;
use fixed_decimal::{Decimal,FloatPrecision,SignedRoundingMode as Signed,UnsignedRoundingMode as Unsigned,RoundingIncrement,SignDisplay};
use icu_decimal::{DecimalFormatter,options::GroupingStrategy};
use icu_plurals::{PluralRules,PluralCategory};
use icu_experimental::dimension::currency::{formatter::CurrencyFormatter,options::{CurrencyFormatterOptions,CurrencyUsage}};
use icu_experimental::dimension::percent::formatter::PercentFormatter;

const MODES:&[&str]=&["ceil","floor","expand","trunc","halfCeil","halfFloor","halfExpand","halfTrunc","halfEven"];
fn number_option(vm:&mut Vm,v:&Value,min:u16,max:u16)->Result<Option<u16>,JsError>{
    if matches!(v,Value::Undefined){return Ok(None)}
    let n=vm.to_number(v)?;
    if !n.is_finite() || n<f64::from(min)||n>f64::from(max){return Err(range("Numeric Intl option out of range"))}
    Ok(Some(n.floor() as u16))
}
#[derive(Clone)]
struct Digits{integer:u16,frac:Option<(u16,u16)>,sig:Option<(u16,u16)>,mode:String,priority:String,trailing:String,increment:u16}
impl Digits{
    fn from_options(vm:&mut Vm,o:&Value,fallback:(u16,u16),compact:bool)->Result<Self,JsError>{
        let int=get(vm,o,"minimumIntegerDigits")?;let integer=number_option(vm,&int,1,21)?.unwrap_or(1);
        let fmin=get(vm,o,"minimumFractionDigits")?;let fmax=get(vm,o,"maximumFractionDigits")?;
        let smin=get(vm,o,"minimumSignificantDigits")?;let smax=get(vm,o,"maximumSignificantDigits")?;
        let inc=get(vm,o,"roundingIncrement")?;let increment=number_option(vm,&inc,1,5000)?.unwrap_or(1);
        if ![1,2,5,10,20,25,50,100,200,250,500,1000,2000,2500,5000].contains(&increment){return Err(range("Invalid rounding increment"))}
        let mode=opt(vm,o,"roundingMode",MODES)?.unwrap_or_else(||"halfExpand".into());
        let mut priority=opt(vm,o,"roundingPriority",&["auto","morePrecision","lessPrecision"])?.unwrap_or_else(||"auto".into());
        let trailing=opt(vm,o,"trailingZeroDisplay",&["auto","stripIfInteger"])?.unwrap_or_else(||"auto".into());
        let has_s=!matches!(smin,Value::Undefined)||!matches!(smax,Value::Undefined);
        let has_f=!matches!(fmin,Value::Undefined)||!matches!(fmax,Value::Undefined);
        let compact_default=compact&&!has_s&&!has_f&&priority=="auto";
        let (mut frac,mut sig)=(None,None);
        if compact_default{frac=Some((0,0));sig=Some((1,2));priority="morePrecision".into()}else{
            if has_s||priority!="auto"{
                let min=number_option(vm,&smin,1,21)?.unwrap_or(1);
                let max=number_option(vm,&smax,min,21)?.unwrap_or(21);
                sig=Some((min,max));
            }
            if !has_s||priority!="auto"{
                let min=number_option(vm,&fmin,0,100)?;let max=number_option(vm,&fmax,0,100)?;
                let min=min.unwrap_or_else(||fallback.0.min(max.unwrap_or(100)));let max=max.unwrap_or_else(||fallback.1.max(min));
                if min>max{return Err(range("Invalid fraction digit range"))}frac=Some((min,max));
            }
        }
        if increment!=1{
            if sig.is_some() {return Err(JsError::type_error("Rounding increments require fraction digits"))}
            if !frac.is_some_and(|(a,b)|a==b){return Err(range("Fraction digits must match with a rounding increment"))}
        }
        Ok(Self{integer,frac,sig,mode,priority,trailing,increment})
    }
    fn round(&self,d:&mut Decimal){
        let mode=match self.mode.as_str(){
            "ceil"=>Signed::Ceil,"floor"=>Signed::Floor,"halfCeil"=>Signed::HalfCeil,"halfFloor"=>Signed::HalfFloor,
            "trunc"=>Signed::Unsigned(Unsigned::Trunc),"expand"=>Signed::Unsigned(Unsigned::Expand),"halfTrunc"=>Signed::Unsigned(Unsigned::HalfTrunc),"halfEven"=>Signed::Unsigned(Unsigned::HalfEven),_=>Signed::Unsigned(Unsigned::HalfExpand)
        };
        let mag=d.nonzero_magnitude_start();
        let sigpos=self.sig.map(|(_,max)|mag.saturating_sub(max as i16-1));let fracpos=self.frac.map(|(_,max)|-(max as i16));
        let use_sig=match(sigpos,fracpos){(Some(a),Some(b))=>if self.priority=="lessPrecision"{a>=b}else{a<=b},(Some(_),None)=>true,_=>false};
        let mut position=if use_sig{sigpos.unwrap()}else{fracpos.unwrap()};
        let mut inc=self.increment;
        while inc>1 && inc%10==0{inc/=10;position=position.saturating_add(1)}
        let incr=match inc {2=>RoundingIncrement::MultiplesOf2,5=>RoundingIncrement::MultiplesOf5,25=>RoundingIncrement::MultiplesOf25,_=>RoundingIncrement::MultiplesOf1};
        d.round_with_mode_and_increment(position,mode,incr);
        d.trim_end();
        let minimum=if use_sig{let (min,_)=self.sig.unwrap();(d.nonzero_magnitude_start()+1-min as i16).min(0)}else{-(self.frac.unwrap().0 as i16)};
        d.pad_end(minimum);
        if self.trailing=="stripIfInteger"{d.trim_end_if_integer()}
        d.pad_start(self.integer as i16);
    }
    fn resolved(&self,vm:&mut Vm,o:&Value)->Result<(),JsError>{
        field(vm,o,"minimumIntegerDigits",usize::from(self.integer))?;
        if let Some((min,max))=self.frac {field(vm,o,"minimumFractionDigits",usize::from(min))?;field(vm,o,"maximumFractionDigits",usize::from(max))?}
        if let Some((min,max))=self.sig {field(vm,o,"minimumSignificantDigits",usize::from(min))?;field(vm,o,"maximumSignificantDigits",usize::from(max))?}
        Ok(())
    }
    fn resolved_round(&self,vm:&mut Vm,o:&Value)->Result<(),JsError>{
        field(vm,o,"roundingIncrement",usize::from(self.increment))?;field(vm,o,"roundingMode",self.mode.as_str())?;
        field(vm,o,"roundingPriority",self.priority.as_str())?;field(vm,o,"trailingZeroDisplay",self.trailing.as_str())
    }
}
fn plural_name(cat:PluralCategory)->&'static str{
    match cat{PluralCategory::Zero=>"zero",PluralCategory::One=>"one",PluralCategory::Two=>"two",PluralCategory::Few=>"few",PluralCategory::Many=>"many",_=>"other"}
}
struct Plural {rules:PluralRules,digits:Digits,locale:String,kind:String}
fn new_plural(vm:&mut Vm,args:&[Value],p:ObjectId)->ResultValue{
    let(l,o)=requested(vm,args,true)?;
    let kind=opt(vm,&o,"type",&["cardinal","ordinal"])?.unwrap_or_else(||"cardinal".into());
    let digits=Digits::from_options(vm,&o,(0,3),false)?;let locale=base(&l);
    let rules=if kind=="ordinal"{PluralRules::try_new_ordinal(l.into())}else{PluralRules::try_new_cardinal(l.into())}.map_err(|_|range("Unsupported plural rules"))?;
    create(vm,p,Plural{rules,digits,locale,kind})
}
fn plural_cat(f:&Plural,n:f64)->&'static str{
    let Ok(mut dec)=Decimal::try_from_f64(n,FloatPrecision::RoundTrip)else{return "other"};
    f.digits.round(&mut dec);
    plural_name(f.rules.category_for(&dec))
}
enum Numeric{Finite(Decimal),Special(&'static str,&'static str)}
fn numeric(vm:&mut Vm,v:&Value)->Result<Numeric,JsError>{
    let p=if v.as_object().is_some(){vm.primitive_hint(v.clone(),"number")?}else{v.clone()};
    let exact=match &p{ Value::BigInt(b)=>Some(b.to_string()),Value::String(s)=>Some(s.to_string_lossy().trim().to_owned()),_=>None};
    if let Some(s)=exact && !s.is_empty() {
        vm.charge_work(s.len())?;
        if let Ok(n)=s.parse::<Decimal>(){return Ok(Numeric::Finite(n))}
    }
    let n=vm.to_number(&p)?;
    if n.is_nan(){return Ok(Numeric::Special("nan","NaN"))}else if n.is_infinite(){return Ok(Numeric::Special("infinity",if n.is_sign_negative(){"-∞"}else{"∞"}))}
    Ok(Numeric::Finite(Decimal::try_from_f64(n,FloatPrecision::RoundTrip).map_err(|_|range("Invalid decimal number"))?))
}
struct NumberFormat{
    patterns: patterns::Patterns, compact_rules:Option<PluralRules>,
    number:DecimalFormatter,currency_fmt:Option<CurrencyFormatter<DecimalFormatter>>,percent:Option<PercentFormatter<DecimalFormatter>>,
    locale:String,numbering:String,style:String,notation:String,sign:String,group:Value,digits:Digits,
    currency:Option<String>,display:String,currency_sign:String,unit:Option<String>,unit_display:String,compact:String,
}
fn valid_unit(s:&str)->bool{
    let mut v=s.split("-per-");let a=v.next().unwrap_or("");data::UNITS.contains(&a)&&v.next().is_none_or(|b|data::UNITS.contains(&b))&&v.next().is_none()
}
fn new_number(vm:&mut Vm,args:&[Value],p:ObjectId)->ResultValue{
    let(mut l,o)=requested(vm,args,true)?;
    let opt_num=opt(vm,&o,"numberingSystem",&[])?;
    if opt_num.as_deref().is_some_and(|s|!unicode_type(s)){return Err(range("Invalid numbering system"))}
    let ext=locale::key(&l,"nu");let mut resolved:Locale=base(&l).parse().unwrap();
    let def_locale:Locale=resolved.clone();
    let number=opt_num.as_ref().filter(|n|data::NUMBERING.contains(&n.as_str())).or_else(||ext.as_ref().filter(|n|data::NUMBERING.contains(&n.as_str()))).cloned().unwrap_or_else(||locale::default_numbering(&def_locale).into());
    locale::set_key(&mut l,"nu",&number)?;
    if ext.as_ref()==Some(&number){locale::set_key(&mut resolved,"nu",&number)?}
    let style=opt(vm,&o,"style",&["decimal","percent","currency","unit"])?.unwrap_or_else(||"decimal".into());
    let currency=opt(vm,&o,"currency",&[])?.map(|s|s.to_ascii_uppercase());
    if currency.as_deref().is_some_and(|s|s.len()!=3||!s.bytes().all(|b|b.is_ascii_uppercase())){return Err(range("Invalid currency code"))}
    if style=="currency"&&currency.is_none(){return Err(JsError::type_error("Currency code required"))}
    let display=opt(vm,&o,"currencyDisplay",&["code","symbol","narrowSymbol","name"])?.unwrap_or_else(||"symbol".into());
    let currency_sign=opt(vm,&o,"currencySign",&["standard","accounting"])?.unwrap_or_else(||"standard".into());
    let unit=opt(vm,&o,"unit",&[])?;
    if unit.as_deref().is_some_and(|s|!valid_unit(s)){return Err(range("Invalid unit"))}
    if style=="unit"&&unit.is_none(){return Err(JsError::type_error("Unit required"))}
    let unit_display=opt(vm,&o,"unitDisplay",&["long","short","narrow"])?.unwrap_or_else(||"short".into());
    let notation=opt(vm,&o,"notation",&["standard","compact","scientific","engineering"])?.unwrap_or_else(||"standard".into());
    let mut cur_digits=2;
    if let Some(c)=&currency {cur_digits=locale::table(data::CURRENCY_DIGITS,c).unwrap_or("2").parse().unwrap_or(2)}
    let defaults=if style=="currency"&&notation=="standard"{(cur_digits,cur_digits)}else if style=="percent"{(0,0)}else{(0,3)};
    let digits=Digits::from_options(vm,&o,defaults,notation=="compact")?;
    let compact=opt(vm,&o,"compactDisplay",&["long","short"])?.unwrap_or_else(||"short".into());
    let g=get(vm,&o,"useGrouping")?;let default_group=if notation=="compact"{"min2"}else{"auto"};
    let group=match g{
        Value::Undefined=>default_group.into(),Value::Bool(true)=>"always".into(),v if !v.is_truthy()=>false.into(),
        v=>{let s=vm.to_string(&v)?.to_string_lossy();match s.as_str(){"true"|"false"=>default_group.into(),"min2"|"always"|"auto"=>s.into(),_=>return Err(range("Invalid grouping"))}}
    };
    let sign=opt(vm,&o,"signDisplay",&["auto","negative","always","never","exceptZero"])?.unwrap_or_else(||"auto".into());
    let grouping=match &group{Value::Bool(false)=>GroupingStrategy::Never,Value::String(s) if s.to_string_lossy()=="min2"=>GroupingStrategy::Min2,Value::String(s) if s.to_string_lossy()=="always"=>GroupingStrategy::Always,_=>GroupingStrategy::Auto};
    let fmt=DecimalFormatter::try_new(l.clone().into(),grouping.into()).map_err(|_|range("Unsupported number format"))?;
    let currency_fmt=if style=="currency"{
        let code=currency.as_ref().unwrap().as_str().parse().map_err(|_|range("Invalid currency"))?;
        let mut opts=CurrencyFormatterOptions::default();
        if currency_sign=="accounting"{opts.usage=CurrencyUsage::Accounting}
        Some(match display.as_str(){
            "code"=>CurrencyFormatter::try_new_code(l.clone().into(),code,opts),"name"=>CurrencyFormatter::try_new_name(l.clone().into(),code),
            "narrowSymbol"=>CurrencyFormatter::try_new_symbol_narrow(l.clone().into(),code,opts),_=>CurrencyFormatter::try_new_symbol(l.clone().into(),code,opts),
        }.map_err(|_|range("Unsupported currency format"))?)
    }else{None};
    let patterns=patterns::Patterns::new(&l,&number);
    let compact_rules=if notation=="compact"{Some(PluralRules::try_new_cardinal(l.clone().into()).map_err(|_|range("Unsupported compact plural rules"))?)}else{None};
    let percent=if style=="percent"{Some(PercentFormatter::try_new(l.into(),Default::default()).map_err(|_|range("Unsupported percent format"))?)}else{None};
    create(vm,p,NumberFormat{patterns,compact_rules,number:fmt,currency_fmt,percent,locale:resolved.to_string(),numbering:number,style,notation,sign,group,digits,currency,display,currency_sign,unit,unit_display,compact})
}
#[derive(Default)]
struct Parts{fields:Vec<(&'static str,String)>,kind:&'static str}
impl fmt::Write for Parts{
    fn write_str(&mut self,s:&str)->fmt::Result{
        if s.is_empty(){return Ok(())}
        let k=if self.kind.is_empty(){"literal"}else{self.kind};
        if let Some((name,v))=self.fields.last_mut()&&*name==k{v.push_str(s)}else{self.fields.push((k,s.into()))}Ok(())
    }
}
impl PartsWrite for Parts{
    type SubPartsWrite=Self;
    fn with_part(&mut self,p:Part,mut f:impl FnMut(&mut Self)->fmt::Result)->fmt::Result{
        let old=self.kind;self.kind=match p.value{"plusSign"|"minusSign"|"integer"|"fraction"|"group"|"decimal"|"currency"|"unit"|"percentSign"|"compact"=>p.value,_=>old};
        let r=f(self);self.kind=old;r
    }
}
fn part_objects(vm:&mut Vm,seq:Vec<(&'static str,String)>,parts:bool)->ResultValue{
    if !parts{return Ok(seq.into_iter().map(|(_,v)|v).collect::<String>().into())}
    let mut arr=Vec::new();for (k,v) in seq{
        let a=vm.new_object();field(vm,&a,"type",k)?;field(vm,&a,"value",v)?;arr.push(a)
    }vm.try_new_array(&arr)
}
impl NumberFormat {
    fn parts(&self,n:Numeric)->Result<Vec<(&'static str,String)>,JsError>{
        let (mut d, special) = match n {
            Numeric::Finite(d) => (d, None),
            Numeric::Special(kind, raw) => {
                let sign_sample = if kind == "nan" { 0 } else if raw.starts_with('-') { -1 } else { 1 };
                (Decimal::from(sign_sample), Some((kind, self.patterns.symbol(kind, raw.trim_start_matches('-')))))
            }
        };
        if self.style=="percent"{d.multiply_pow10(2)}
        let scientific=matches!(self.notation.as_str(),"engineering"|"scientific") && special.is_none();
        let mut exponent=0;
        let mut compact_pattern=None;
        if let Some(rules)=&self.compact_rules && special.is_none(){
            compact_pattern=self.patterns.compact(&self.compact,&mut d,&self.digits,rules);
        } else {
            if scientific && !d.is_zero(){
                exponent=d.nonzero_magnitude_start();
                if self.notation=="engineering"{exponent=exponent.div_euclid(3)*3}
                d.multiply_pow10(-exponent);
            }
            self.digits.round(&mut d);
        }
        let sign=match self.sign.as_str(){"always"=>SignDisplay::Always,"never"=>SignDisplay::Never,"exceptZero"=>SignDisplay::ExceptZero,"negative"=>SignDisplay::Negative,_=>SignDisplay::Auto};
        let d=d.with_sign_display(sign);
        let mut parts=Parts::default();
        if let Some(c)=&self.currency_fmt{c.format_fixed_decimal(&d).write_to_parts(&mut parts).unwrap()}
        else if let Some(p)=&self.percent{p.format(&d).write_to_parts(&mut parts).unwrap()}
        else {self.number.format(&d).write_to_parts(&mut parts).unwrap()}
        let is_number_part=|(k,_):&(&str,String)|matches!(*k,"integer"|"group"|"fraction"|"decimal");
        if let Some(first)=parts.fields.iter().position(is_number_part){
            let end=parts.fields.iter().rposition(is_number_part).unwrap()+1;
            if let Some((kind,symbol))=special{
                parts.fields.splice(first..end,[(kind,symbol.to_owned())]);
            }else if let Some(pattern)=compact_pattern{
                let (before,show_value,after)=patterns::affixes(pattern);
                parts.fields.splice(end..end,after);
                parts.fields.splice(first..if show_value{first}else{end},before);
            }
        }
        if scientific {
            parts.fields.push(("exponentSeparator",self.patterns.symbol("exponential","E").to_owned()));
            if exponent<0{parts.fields.push(("exponentMinusSign",self.patterns.symbol("minusSign","-").to_owned()))}
            parts.fields.push(("exponentInteger",exponent.unsigned_abs().to_string()));
        }
        for (kind,text) in &mut parts.fields{
            if matches!(*kind,"integer"|"fraction"|"exponentInteger"){self.patterns.shape_digits(text)}
        }
        if self.style=="unit"&&let Some(unit)=&self.unit{
            let sym=match(unit.as_str(),self.unit_display.as_str()){("percent",_)=>"%",("meter","short"|"narrow")=>"m",("kilometer","short"|"narrow")=>"km",("second","short"|"narrow")=>"s",("hour","short"|"narrow")=>"hr",_=>unit};
            if self.unit_display!="narrow"{parts.fields.push(("literal"," ".into()))}parts.fields.push(("unit",sym.into()))
        }
        Ok(parts.fields)
    }
    fn resolved(&self,vm:&mut Vm)->ResultValue{
        let o=vm.new_object();
        for (k,s) in [("locale",&self.locale),("numberingSystem",&self.numbering),("style",&self.style)]{field(vm,&o,k,s.as_str())?}
        if self.style=="currency"{field(vm,&o,"currency",self.currency.as_deref().unwrap())?;field(vm,&o,"currencyDisplay",self.display.as_str())?;field(vm,&o,"currencySign",self.currency_sign.as_str())?}
        if self.style=="unit"{field(vm,&o,"unit",self.unit.as_deref().unwrap())?;field(vm,&o,"unitDisplay",self.unit_display.as_str())?}
        self.digits.resolved(vm,&o)?;field(vm,&o,"useGrouping",self.group.clone())?;field(vm,&o,"notation",self.notation.as_str())?;
        if self.notation=="compact"{field(vm,&o,"compactDisplay",self.compact.as_str())?}
        field(vm,&o,"signDisplay",self.sign.as_str())?;self.digits.resolved_round(vm,&o)?;Ok(o)
    }
}
pub(super) fn install(vm:&mut Vm,intl:Value){
    let (ctor,p)=class(vm,intl.clone(),"PluralRules",0,false,new_plural);supported(vm,&ctor);
    method(vm,p.clone(),"select",1,|vm,this,args|{let f=slots::<Plural>(vm,&this)?;Ok(plural_cat(&f,vm.to_number(&arg(args,0))?).into())});
    method(vm,p.clone(),"selectRange",2,|vm,this,args|{
        let f=slots::<Plural>(vm,&this)?;
        if args.len()<2 || args[..2].iter().any(|v|matches!(v,Value::Undefined)){return Err(JsError::type_error("Both numbers are required"))}
        let a=vm.to_number(&args[0])?;let b=vm.to_number(&args[1])?;if a.is_nan()||b.is_nan(){return Err(range("Invalid range endpoint"))}
        let c1=plural_cat(&f,a);let c2=plural_cat(&f,b);Ok(locale::table(data::PLURAL_RANGES,&format!("{}:{c1}:{c2}",f.locale.split('-').next().unwrap_or("en"))).unwrap_or("other").into())
    });
    method(vm,p,"resolvedOptions",0,|vm,this,_|{
        let f=slots::<Plural>(vm,&this)?;let o=vm.new_object();field(vm,&o,"locale",f.locale.as_str())?;field(vm,&o,"type",f.kind.as_str())?;f.digits.resolved(vm,&o)?;
        let cats = f.rules.categories().map(plural_name);
        let arr=strings(vm,cats)?;field(vm,&o,"pluralCategories",arr)?;f.digits.resolved_round(vm,&o)?;Ok(o)
    });
    let (ctor,p)=class(vm,intl,"NumberFormat",0,true,new_number);supported(vm,&ctor);
    accessor(vm,&p,"format",|vm,this,_|{
        let this=unwrap_formatter::<NumberFormat>(vm,this,"NumberFormat")?;
        let fmt=slots::<NumberFormat>(vm,&this)?;if let Some(f)=vm.host_slot(&this,0){return Ok(f)}
        let f=vm.native("",1,false,Rc::new(move|vm,_,args|{let n=numeric(vm,&arg(args,0))?;part_objects(vm,fmt.parts(n)?,false)}));
        vm.set_host_slot(this,0,f.clone())?;Ok(f)
    });
    method(vm,p.clone(),"formatToParts",1,|vm,this,args|{let fmt=slots::<NumberFormat>(vm,&this)?;let n=numeric(vm,&arg(args,0))?;part_objects(vm,fmt.parts(n)?,true)});
    method(vm,p.clone(),"resolvedOptions",0,|vm,this,_|{
        let this=unwrap_formatter::<NumberFormat>(vm,this,"NumberFormat")?;
        slots::<NumberFormat>(vm,&this)?.resolved(vm)
    });
    for name in ["formatRange","formatRangeToParts"]{
        method(vm,p.clone(),name,2,move|vm,this,args|{
            let f=slots::<NumberFormat>(vm,&this)?;
            if args.len()<2||args[..2].iter().any(|v|matches!(v,Value::Undefined)){return Err(JsError::type_error("Both numbers are required"))}
            let a=numeric(vm,&args[0])?;let b=numeric(vm,&args[1])?;
            if matches!(a,Numeric::Special("nan",_))||matches!(b,Numeric::Special("nan",_)){return Err(range("Invalid range endpoint"))}
            let a=f.parts(a)?;let b=f.parts(b)?;let equal=a==b;
            let mut out=Vec::new();let mut text=String::new();
            let sequences=if equal{vec![(vec![("approximatelySign","~".into())],"shared"),(a,"shared")]}else{vec![(a,"startRange"),(vec![("literal","–".into())],"shared"),(b,"endRange")]};
            for(seq,source)in sequences{for(k,v)in seq{if name=="formatRange"{text.push_str(&v)}else{let obj=vm.new_object();field(vm,&obj,"type",k)?;field(vm,&obj,"value",v)?;field(vm,&obj,"source",source)?;out.push(obj)}}}
            if name=="formatRange"{Ok(text.into())}else{vm.try_new_array(&out)}
        });
    }
    for (name,proto) in [("Number",Some(vm.realm.number_prototype)),("BigInt",vm.realm.bigint_prototype)]{
        if let Some(proto)=proto{method(vm,Value::Object(proto),"toLocaleString",0,move|vm,this,args|{
            let value=match &this {
                Value::Number(_) if name=="Number"=>this,
                Value::BigInt(_) if name=="BigInt"=>this,
                Value::Object(id)=> if let ObjectKind::Boxed(v)=&vm.heap.object(*id)?.kind && ((name=="BigInt"&&matches!(v,Value::BigInt(_)))||(name=="Number"&&matches!(v,Value::Number(_)))) {v.clone()}else{return Err(JsError::type_error("Incompatible number receiver"))},
                _=>return Err(JsError::type_error("Incompatible number receiver"))
            };
            let fmt=new_number(vm,args,prototype(vm,"NumberFormat"))?;
            let n=numeric(vm,&value)?;part_objects(vm,slots::<NumberFormat>(vm,&fmt)?.parts(n)?,false)
        });}
    }
}
