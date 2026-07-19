#!/usr/bin/env python3
import csv, math, re, subprocess, time
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
EXP=ROOT/'experiment'
MINIGU=ROOT/'target/release/minigu'
PATTERNS=EXP/'patterns/gcard/aids_merged_predicates'
TRUTH=EXP/'result/truecard/aids_merged_predicates/true_card_ok.csv'
RUN_ROOT=EXP/'result/gcard_query/aids_merged_predicates_k2_d5'
TMP=EXP/'run_tmp/aids_merged_predicates_gcard/query_scale.gql'
DB=EXP/'run_tmp/aids_merged_predicates_gcard/db'
GRAPH='aids_merged_predicates'
CARD_RE=re.compile(r",\s*([^,\s]+),\s*cardinality:\s*([0-9.eE+-]+)")
TIME_RE=re.compile(r"Time:\s*([0-9.]+)s")
PROF_RE=re.compile(r"total: (?P<total>\d+).*sample_time: (?P<sample>[0-9.]+), estimate_time: (?P<estimate>[0-9.]+), build_time: (?P<build>[0-9.]+).*cardinality: (?P<cardinality>[0-9.]+)")
def qerror(est, truth):
    est=max(float(est),1.0); truth=max(float(truth),1.0); return max(est/truth, truth/est)
def parse_profiles(log):
    profiles=[]; cli=[]; current=None
    for line in log.read_text(encoding='utf-8',errors='replace').splitlines():
        if line.startswith('[build-prof]'):
            current={k:float(v) for k,v in re.findall(r"([a-zA-Z_+]+): ([0-9.]+)s",line)}
        elif line.startswith('total:'):
            m=PROF_RE.search(line)
            if m:
                current=current or {}
                current.update({'gcard_total_candidates':float(m.group('total')),'sample_time_s':float(m.group('sample')),'estimate_time_s':float(m.group('estimate')),'build_time_s':float(m.group('build'))})
                profiles.append(current); current=None
        elif line.startswith('Time:'):
            m=TIME_RE.search(line)
            if m: cli.append(float(m.group(1)))
    return profiles,cli
rows=list(csv.DictReader(TRUTH.open(newline='',encoding='utf-8')))
patterns=[PATTERNS/(r['pattern']+'.json') for r in rows]
RUN_ROOT.mkdir(parents=True,exist_ok=True); TMP.parent.mkdir(parents=True,exist_ok=True)
lines=[f'session set graph {GRAPH}',f'call load_catalog("{GRAPH}")','call set_gcard_star_config(1, 5)']
for p in patterns:
    lines.append(f':time call gcard_query("{p}", 2, 500, 1, false, 10)')
lines.append(':quit')
TMP.write_text('\n'.join(lines)+'\n',encoding='utf-8')
log=RUN_ROOT/'aids_merged_predicates_scale.log'
with log.open('w',encoding='utf-8') as out:
    proc=subprocess.run([str(MINIGU),'execute',str(TMP),'--path',str(DB)],cwd=str(ROOT),stdout=out,stderr=subprocess.STDOUT,text=True)
if proc.returncode!=0:
    raise SystemExit(f'minigu exited {proc.returncode}; see {log}')
text=log.read_text(encoding='utf-8',errors='replace')
est=[float(m.group(2)) for m in CARD_RE.finditer(text)]
if len(est)!=len(rows):
    raise SystemExit(f'got {len(est)} estimates expected {len(rows)}; see {log}')
profiles,cli=parse_profiles(log)
out=RUN_ROOT/'detail_scale.csv'
fields=['dataset','topology','query','pattern','estimate','true','qerror','latency_s','build_time_s','estimate_time_s','sample_time_s','cli_time_s','gcard_total_candidates']
with out.open('w',newline='',encoding='utf-8') as f:
    w=csv.DictWriter(f,fieldnames=fields); w.writeheader()
    for i,(r,e) in enumerate(zip(rows,est)):
        prof=profiles[i] if i < len(profiles) else {}
        latency=(prof.get('build_time_s',0)+prof.get('estimate_time_s',0)+prof.get('sample_time_s',0)) if prof else (cli[i] if i < len(cli) else '')
        w.writerow({'dataset':'aids_merged_predicates','topology':r['topology'],'query':r['query'],'pattern':r['pattern'],'estimate':f'{e:.17g}','true':r['true_cardinality'],'qerror':f'{qerror(e,r["true_cardinality"]):.17g}','latency_s':f'{latency:.9g}' if isinstance(latency,float) else latency,'build_time_s':prof.get('build_time_s',''),'estimate_time_s':prof.get('estimate_time_s',''),'sample_time_s':prof.get('sample_time_s',''),'cli_time_s':f'{cli[i]:.9g}' if i < len(cli) else '','gcard_total_candidates':prof.get('gcard_total_candidates','')})
print('detail_scale=',out)
print('log=',log)
print('rows=',len(rows))
