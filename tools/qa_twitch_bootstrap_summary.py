import sys
from collections import defaultdict
from pathlib import Path
rows=Path(sys.argv[1]).read_text().splitlines()
print("mode:",sys.argv[1])
for l in rows:
    if l.startswith(("SUMMARY", "CORE elapsed", "test result:", "	Maximum resident set size")):print(l)
print("GC wall:",round(sum(float(s[3:]) for s in rows if s.startswith("GC ")),3))
groups=defaultdict(list)
for s in rows:
    if s.startswith("COST "):
        _,kind,loc,total,count=s.split()
        groups[kind].append((int(total),loc,int(count)))
for group,stats in groups.items():
    print("group",group,"total",sum(x[0] for x in stats),"top",sorted(stats,reverse=True)[:9])
stack=[]
mods=[]
for line in rows:
    if not line.startswith("FACTORY "):continue
    _,enter,mod,fuel,mem,cells=line.split()
    fuel=int(fuel);mem=int(mem)
    if enter=="1": stack.append([mod,fuel,mem,0,0])
    elif stack and stack[-1][0]==mod:
        m,f,b,child_f,child_b=stack.pop();inc_f=f-fuel;inc_b=mem-b
        mods.append((m,inc_f-child_f,inc_b-child_b))
        if stack: stack[-1][3]+=inc_f;stack[-1][4]+=inc_b
print("completed factories",len(mods),"active ancestry",[m[0] for m in stack][-20:])
if mods:print("most exclusive fuel",sorted(mods,key=lambda v:-v[1])[:12],"most exclusive bytes",sorted(mods,key=lambda v:-v[2])[:12])
