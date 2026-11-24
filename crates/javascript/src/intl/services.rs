use super::*;
use crate::JsString;
use icu_collator::{Collator,CollatorBorrowed,options::{CollatorOptions,Strength,CaseLevel,AlternateHandling}};
use icu_list::{ListFormatter,options::{ListLength,ListFormatterOptions}};
use std::cell::Cell;
use unicode_segmentation::UnicodeSegmentation;
use writeable::{PartsWrite,Part,Writeable};
use std::fmt;
mod numeric;
mod time;
mod display;
fn requested(vm:&mut Vm,args:&[Value],coerce:bool)->Result<(Locale,Value),JsError>{
    let ll=locale::list(vm,arg(args,0))?;
    let opts=options(vm,arg(args,1),coerce)?;
    opt(vm,&opts,"localeMatcher",&["lookup","best fit"])?;
    Ok((resolve(&ll),opts))
}
fn base(l:&Locale)->String { locale::lookup(l).unwrap_or_else(||"en-US".into()) }
pub(super) fn unicode_type(s:&str)->bool {s.split('-').all(|p|(3..=8).contains(&p.len())&&p.bytes().all(|c|c.is_ascii_alphanumeric()))}
struct Collation{coll:CollatorBorrowed<'static>, locale:String,usage:String,co:String,sensitivity:String,numeric:bool,first:String,punctuation:bool}
fn new_collator(vm:&mut Vm,args:&[Value],proto:ObjectId)->ResultValue{
    let ll=locale::list(vm,arg(args,0))?;
    let opts=options(vm,arg(args,1),true)?;
    let usage=opt(vm,&opts,"usage",&["sort","search"])?.unwrap_or_else(||"sort".into());
    opt(vm,&opts,"localeMatcher",&["lookup","best fit"])?;
    let mut l=resolve(&ll);
    let mut resolved:Locale=base(&l).parse().unwrap();
    let opt_co=opt(vm,&opts,"collation",&[])?;
    if opt_co.as_deref().is_some_and(|s|!unicode_type(s)){return Err(range("Invalid collation type"))}
    let extension_co=locale::key(&l,"co");
    let mut co=opt_co.clone().or(extension_co.clone()).unwrap_or_else(||"default".into());
    if usage=="search" || !data::COLLATIONS.contains(&co.as_str()) {co="default".into()}
    if co!="default" && opt_co.is_none(){locale::set_key(&mut resolved,"co",&co)?}
    let v=get(vm,&opts,"numeric")?;
    let extension_numeric=locale::key(&l,"kn");
    let numeric=if matches!(v,Value::Undefined){extension_numeric.as_deref().is_some_and(|s|s.is_empty()||s=="true")}else{v.is_truthy()};
    if matches!(v,Value::Undefined) && let Some(s)=extension_numeric && matches!(s.as_str(),""|"true"|"false"){locale::set_key(&mut resolved,"kn",if numeric{"true"}else{"false"})?;}
    let opt_first=opt(vm,&opts,"caseFirst",&["upper","lower","false"])?;
    let ext_first=locale::key(&l,"kf").filter(|s|matches!(s.as_str(),"upper"|"lower"|"false"));
    if opt_first.is_none()&&let Some(s)=&ext_first{locale::set_key(&mut resolved,"kf",s)?}
    let first=opt_first.or(ext_first).unwrap_or_else(||"false".into());
    let sensitivity=opt(vm,&opts,"sensitivity",&["base","accent","case","variant"])?.unwrap_or_else(||"variant".into());
    let p=get(vm,&opts,"ignorePunctuation")?;let punctuation=if matches!(p,Value::Undefined){l.id.language.as_str()=="th"}else{p.is_truthy()};
    locale::set_key(&mut l,"kn",if numeric{"true"}else{"false"})?;
    locale::set_key(&mut l,"kf",&first)?;
    if usage=="search"{locale::set_key(&mut l,"co","search")?}else if co!="default"{locale::set_key(&mut l,"co",&co)?}
    let mut c=CollatorOptions::default();c.strength=Some(match sensitivity.as_str(){"base"|"case"=>Strength::Primary,"accent"=>Strength::Secondary,_=>Strength::Tertiary});
    if sensitivity=="case"{c.case_level=Some(CaseLevel::On)}
    c.alternate_handling=Some(if punctuation {AlternateHandling::Shifted}else{AlternateHandling::NonIgnorable});
    let coll=Collator::try_new(l.into(),c).map_err(|_|range("Unsupported collation"))?;
    create(vm,proto,Collation{coll,locale:resolved.to_string(),usage,co,sensitivity,numeric,first,punctuation})
}
struct List {format:ListFormatter,locale:String,style:String,kind:String}
fn new_list(vm:&mut Vm,args:&[Value],proto:ObjectId)->ResultValue{
    let (l,opts)=requested(vm,args,false)?;
    let kind=opt(vm,&opts,"type",&["conjunction","disjunction","unit"])?.unwrap_or_else(||"conjunction".into());
    let style=opt(vm,&opts,"style",&["long","short","narrow"])?.unwrap_or_else(||"long".into());
    let options=ListFormatterOptions::default().with_length(match style.as_str(){"short"=>ListLength::Short,"narrow"=>ListLength::Narrow,_=>ListLength::Wide});
    let locale=base(&l);
    let format=match kind.as_str(){"disjunction"=>ListFormatter::try_new_or(l.into(),options),"unit"=>ListFormatter::try_new_unit(l.into(),options),_=>ListFormatter::try_new_and(l.into(),options)}.map_err(|_|range("Unsupported list format"))?;
    create(vm,proto,List{format,locale,style,kind})
}
#[derive(Default)]
struct ListParts{fields:Vec<(&'static str,String)>,current:Option<&'static str>}
impl fmt::Write for ListParts{
    fn write_str(&mut self,s:&str)->fmt::Result{
        let kind = self.current.unwrap_or("literal");
        if kind == "literal" {
            if s.is_empty() {
                return Ok(());
            }
            if let Some(("literal", previous)) = self.fields.last_mut() {
                previous.push_str(s);
                return Ok(());
            }
        }
        self.fields.push((kind, s.into()));
        Ok(())
    }
}
impl PartsWrite for ListParts{
    type SubPartsWrite=Self;
    fn with_part(&mut self,p:Part,mut f:impl FnMut(&mut Self)->fmt::Result)->fmt::Result{
        let old=self.current;self.current=Some(p.value);let r=f(self);self.current=old;r
    }
}
fn list_values(vm:&mut Vm,val:Value)->Result<Vec<JsString>,JsError>{
    let mut list=Vec::new();
    if matches!(val,Value::Undefined){return Ok(list)}
    let mut it=vm.get_iterator(val)?;
    while let Some(value)=vm.iterator_step(&mut it)?{
        vm.charge_work(1)?;
        if let Value::String(s)=value{list.push(s)}else {
            return vm.iterator_close(&mut it,Err(JsError::type_error("List elements must be strings"))).map(|_|Vec::new())
        }
    }
    Ok(list)
}
struct Segmenter{granularity:String,locale:String}
fn new_segmenter(vm:&mut Vm,args:&[Value],p:ObjectId)->ResultValue{
    let (l,o)=requested(vm,args,false)?;
    let granularity=opt(vm,&o,"granularity",&["grapheme","word","sentence"])?.unwrap_or_else(||"grapheme".into());
    create(vm,p,Segmenter{granularity,locale:base(&l)})
}
struct Segments{input:JsString, ends:Vec<usize>, word:Option<Vec<bool>>}
struct SegIter{segments:Rc<Segments>,index:Cell<usize>}
impl Segments{
    fn record(&self,vm:&mut Vm,i:usize)->ResultValue{
        let start=i.checked_sub(1).map_or(0,|i|self.ends[i]);
        let o=vm.new_object();
        field(vm,&o,"segment",JsString::from_utf16(self.input.as_utf16()[start..self.ends[i]].to_vec()))?;
        field(vm,&o,"index",start)?;field(vm,&o,"input",self.input.clone())?;
        if let Some(words)=&self.word{field(vm,&o,"isWordLike",words[i])?}
        Ok(o)
    }
}
fn install_segmenter(vm:&mut Vm,intl:Value){
    let (ctor,p)=class(vm,intl,"Segmenter",0,false,new_segmenter);supported(vm,&ctor);
    let seg=vm.new_object();let iter=vm.new_object_with_prototype(vm.realm.iterator_prototype.map(Value::Object).unwrap_or_else(||vm.global())).unwrap();
    for (n,o) in [("Segments",seg.clone()),("Segment Iterator",iter.clone())]{vm.realm.intl_prototypes.push((n,o.as_object().unwrap()));}
    tag(vm,iter.clone(),"Segmenter String Iterator");
    let it=vm.native("[Symbol.iterator]",0,false,Rc::new(|vm,this,_|{
        let segments=slots::<Segments>(vm,&this)?;
        create(vm,prototype(vm,"Segment Iterator"),SegIter{segments,index:Cell::new(0)})
    }));
    vm.define_data_property_key(seg.clone(),vm.well_known_key("iterator"),it,true,false,true).unwrap();
    method(vm,seg,"containing",1,|vm,this,args|{
        let seg=slots::<Segments>(vm,&this)?;
        let n=vm.to_number(&arg(args,0))?;let n=if n.is_nan(){0.}else{n.trunc()};
        if n<0.||n>=seg.input.len() as f64{return Ok(Value::Undefined)}
        seg.record(vm,seg.ends.partition_point(|&e|e<=n as usize))
    });
    method(vm,iter,"next",0,|vm,this,_|{
        let it=slots::<SegIter>(vm,&this)?;let i=it.index.get();let done=i==it.segments.ends.len();
        let v=if done{Value::Undefined}else{it.index.set(i+1);it.segments.record(vm,i)?};
        let o=vm.new_object();field(vm,&o,"value",v)?;field(vm,&o,"done",done)?;Ok(o)
    });
    method(vm,p.clone(),"resolvedOptions",0,|vm,this,_|{
        let s=slots::<Segmenter>(vm,&this)?;let o=vm.new_object();field(vm,&o,"locale",s.locale.as_str())?;field(vm,&o,"granularity",s.granularity.as_str())?;Ok(o)
    });
    method(vm,p,"segment",1,|vm,this,args|{
        let s=slots::<Segmenter>(vm,&this)?;
        let input=vm.to_string(&arg(args,0))?;
        vm.charge_work(input.len())?;
        vm.reserve_allocation(input.len().saturating_mul(std::mem::size_of::<usize>()+1))?;
        let text=input.to_string_lossy();
        let is_word=s.granularity=="word";
        let chunks:Box<dyn Iterator<Item=&str>>=match s.granularity.as_str(){"word"=>Box::new(text.split_word_bounds()),"sentence"=>Box::new(text.split_sentence_bounds()),_=>Box::new(text.graphemes(true))};
        let mut ends=Vec::new();let mut word=is_word.then(Vec::new);let mut offset=0;
        for chunk in chunks{offset+=chunk.encode_utf16().count();ends.push(offset);if let Some(word)=&mut word{word.push(chunk.unicode_words().next().is_some());}}
        create(vm,prototype(vm,"Segments"),Segments{input,ends,word})
    });
}
fn install_string_methods(vm: &mut Vm) {
    let p=Value::Object(vm.realm.string_prototype);
    method(vm,p.clone(),"localeCompare",1,|vm,this,args|{
        if this.is_nullish(){return Err(JsError::type_error("String method requires a receiver"))}
        let a=vm.to_string(&this)?;let b=vm.to_string(&arg(args,0))?;
        let coll=new_collator(vm,args.get(1..).unwrap_or_default(),prototype(vm,"Collator"))?;
        let f=slots::<Collation>(vm,&coll)?; vm.charge_work(a.len()+b.len())?;
        Ok((f.coll.compare_utf16(a.as_utf16(),b.as_utf16()) as i32).into())
    });
    for (name,upper) in [("toLocaleLowerCase",false),("toLocaleUpperCase",true)]{
        method(vm,p.clone(),name,0,move|vm,this,args|{
            if this.is_nullish(){return Err(JsError::type_error("String method requires a receiver"))}
            let s=vm.to_string(&this)?;
            let locales=locale::list(vm,arg(args,0))?;
            let locale=locales.first().cloned().unwrap_or_else(||icu_locale::locale!("en-US"));
            let mapper=icu_casemap::CaseMapper::new();
            vm.charge_work(s.len())?;
            let mut result=Vec::with_capacity(s.len());
            let mut valid=String::new();
            let flush=|s:&mut String, out:&mut Vec<u16>|{
                let mapped=if upper{mapper.uppercase_to_string(s,&locale.id)}else{mapper.lowercase_to_string(s,&locale.id)};
                out.extend(mapped.encode_utf16());s.clear();
            };
            for c in char::decode_utf16(s.as_utf16().iter().copied()){
                match c{Ok(ch)=>valid.push(ch),Err(err)=>{flush(&mut valid,&mut result);result.push(err.unpaired_surrogate());}}
            }
            flush(&mut valid,&mut result);
            vm.finish_string(result)
        });
    }
}
pub(super) fn install(vm:&mut Vm,intl:Value){
    let (coll,p)=class(vm,intl.clone(),"Collator",0,true,new_collator);supported(vm,&coll);
    accessor(vm,&p,"compare",|vm,this,_|{
        let c=slots::<Collation>(vm,&this)?;
        if let Some(f)=vm.host_slot(&this,0){return Ok(f)}
        let f=vm.native("",2,false,Rc::new(move|vm,_,args|{
            let a=vm.to_string(&arg(args,0))?;let b=vm.to_string(&arg(args,1))?;vm.charge_work(a.len()+b.len())?;
            Ok((c.coll.compare_utf16(a.as_utf16(),b.as_utf16()) as i32).into())
        }));
        vm.set_host_slot(this,0,f.clone())?;Ok(f)
    });
    method(vm,p,"resolvedOptions",0,|vm,this,_|{
        let c=slots::<Collation>(vm,&this)?;let o=vm.new_object();
        for (k,v) in [("locale",&c.locale),("usage",&c.usage),("sensitivity",&c.sensitivity)]{field(vm,&o,k,v.as_str())?}
        field(vm,&o,"ignorePunctuation",c.punctuation)?;field(vm,&o,"collation",c.co.as_str())?;
        field(vm,&o,"numeric",c.numeric)?;field(vm,&o,"caseFirst",c.first.as_str())?;Ok(o)
    });
    let (list,p)=class(vm,intl.clone(),"ListFormat",0,false,new_list);supported(vm,&list);
    method(vm,p.clone(),"resolvedOptions",0,|vm,this,_|{
        let c=slots::<List>(vm,&this)?;let o=vm.new_object();
        for(k,v)in[("locale",&c.locale),("type",&c.kind),("style",&c.style)]{field(vm,&o,k,v.as_str())?} Ok(o)
    });
    for name in ["format","formatToParts"]{
        method(vm,p.clone(),name,1,move|vm,this,args|{
            let c=slots::<List>(vm,&this)?;
            let vals=list_values(vm,arg(args,0))?;
            let v:Vec<_>=vals.iter().map(JsString::to_string_lossy).collect();
            let mut out=ListParts::default();c.format.format(v.iter()).write_to_parts(&mut out).unwrap();
            // Formatting patterns depend on text, but the inserted strings retain
            // their exact UTF-16 code units, including isolated surrogates.
            let mut elements=vals.into_iter();
            let mut joined=Vec::new();let mut arr=Vec::new();
            for (kind,part) in out.fields{
                let s=if kind=="element"{elements.next().unwrap_or_default()}else{JsString::from(part)};
                if name=="format"{joined.extend_from_slice(s.as_utf16())}else{
                    let o=vm.new_object();field(vm,&o,"type",kind)?;field(vm,&o,"value",s)?;arr.push(o)
                }
            }
            if name=="format"{vm.finish_string(joined)}else{vm.try_new_array(&arr)}
        });
    }
    install_string_methods(vm);
    install_segmenter(vm,intl.clone());
    display::install(vm,intl.clone());
    time::install(vm,intl.clone());
    numeric::install(vm,intl);
}
