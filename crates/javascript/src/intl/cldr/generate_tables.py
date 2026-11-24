"""Rebuild the ECMA-402 tables from cldr-core and cldr-bcp47 49.0.0 and IANA TZDB 2026c zone.tab.

Pass a directory containing the two unpacked npm packages and the path to zone.tab.
"""
from pathlib import Path
import argparse
import collections
import hashlib
import json

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("cldr", type=Path)
parser.add_argument("zones", type=Path)
args = parser.parse_args()
src = args.cldr
target = Path(__file__).resolve().parent.parent
for package in ["cldr-core", "cldr-bcp47"]:
    if json.loads((src / package / "package.json").read_text())["version"] != "49.0.0":
        raise ValueError(f"Expected {package} 49.0.0")
if hashlib.sha256(args.zones.read_bytes()).hexdigest() != "7cc78ea166261b3dedf951cdd721051460851e6fcd96c12b8e3194cf25677f21":
    raise ValueError("Expected IANA TZDB 2026c zone.tab")

out=["// Unicode CLDR 49 supplemental data and BCP 47 registry, Unicode License v3.\n// IANA country-to-zone associations from public domain zone.tab.\n"]
def read(n):return json.loads((src/n).read_text())
def supp(n):return read(f'cldr-core/supplemental/{n}.json')['supplemental'][n]
def bcp(n,k):return {a:v for a,v in read(f'cldr-bcp47/bcp47/{n}.json')['keyword']['u'][k].items() if not a.startswith('_') and not v.get('_deprecated')}
def array(name,z):out.append(f'pub(super) const {name}: &[&str] = &[\n'+''.join('    '+json.dumps(t)+',\n' for t in sorted(set(z)))+'];\n')
def table(name,d):out.append(f'pub(super) const {name}: &[(&str,&str)] = &[\n'+''.join(f'    ({json.dumps(k,ensure_ascii=False)},{json.dumps(v,ensure_ascii=False)}),\n' for k,v in sorted(d.items()))+'];\n')
array('LOCALES',read('cldr-core/availableLocales.json')['availableLocales']['full']+read('cldr-core/defaultContent.json')['defaultContent'])
array('CALENDARS',[k for k in bcp('calendar','ca') if k not in ('islamic-rgsa',)])
array('COLLATIONS',[k for k in bcp('collation','co') if k not in ('standard','search','ducet')])
curr=supp('currencyData')['region']
array('CURRENCIES',[k for rows in curr.values() for item in rows for k,val in item.items() if '_to' not in val and val.get('_tender')!='false'])
array('NUMBERING',[k for k,v in supp('numberingSystems').items() if '_digits' in v])
array('UNITS','acre bit byte celsius centimeter day degree fahrenheit fluid-ounce foot gallon gigabit gigabyte gram hectare hour inch kilobit kilobyte kilogram kilometer liter megabit megabyte meter microsecond mile mile-scandinavian milliliter millimeter millisecond minute month nanosecond ounce percent petabyte pound second stone terabit terabyte week yard year'.split())
zones={};regionzones=collections.defaultdict(set)
for k,v in bcp('timezone','tz').items():
 aliases=v.get('_alias','').split()
 if not aliases:continue
 primary=v.get('_iana',aliases[0]);primary='UTC' if primary in ('Etc/UTC','Etc/GMT') else primary
 for alias in aliases+[primary]:zones[alias.lower()]=primary
for row in args.zones.read_text().splitlines():
 if row.startswith('#') or not row:continue
 reg,_,zone,*_=row.split('\t'); regionzones[reg].add(zones.get(zone.lower(),zone))
array('ZONES',zones.values()); table('ZONE_ALIASES',zones);table('REGION_ZONES',{k:' '.join(sorted(v)) for k,v in regionzones.items()})
week=supp('weekData')
for k,n in [('firstDay','FIRST_DAY'),('weekendStart','WEEKEND_START'),('weekendEnd','WEEKEND_END')]:
 table(n,{k:v for k,v in week[k].items() if '-' not in k})
table('HOURS',{k: {'H':'h23','h':'h12','K':'h11','k':'h24'}[v['_preferred']] for k,v in supp('timeData').items()})
table('CALENDAR_PREFS',{k:' '.join('gregory' if c=='gregorian' else c for c in v) for k,v in supp('calendarPreferenceData').items()})

numbering = supp('numberingSystems')
table('DIGIT_ZERO', {v['_digits'][0]: k for k, v in numbering.items() if '_digits' in v})
table('CURRENCY_DIGITS', {k: v['_digits'] for k, v in supp('currencyData')['fractions'].items()})
ranges = read('cldr-core/supplemental/pluralRanges.json')['supplemental']['plurals']
pairs = {}
for loc, entries in ranges.items():
    for name, value in entries.items():
        _, _, start, _, end = name.split('-')
        pairs[f'{loc}:{start}:{end}'] = value
table('PLURAL_RANGES', pairs)
aliases = {}
for path in sorted((src / 'cldr-bcp47/bcp47').glob('*.json')):
    groups = json.loads(path.read_text())['keyword'].get('u', {})
    for key, values in groups.items():
        for code, record in values.items():
            if not isinstance(record, dict):
                continue
            preferred = record.get('_preferred', code)
            if preferred != code:
                aliases[f'{key}:{code}'] = preferred
            for alias in record.get('_alias', '').lower().split():
                if all(3 <= len(s) <= 8 and s.isalnum() for s in alias.split('-')) and preferred != alias:
                    aliases[f'{key}:{alias}'] = preferred
table('UNICODE_ALIASES', aliases)
(target/'data.rs').write_text('\n'.join(out))
(target/'UNICODE-LICENSE.txt').write_bytes((src/'cldr-core/LICENSE').read_bytes())
