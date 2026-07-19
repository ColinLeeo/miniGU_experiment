#!/usr/bin/env python3
import csv, math, re, subprocess
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
EXP=ROOT/'experiment'
MINIGU=ROOT/'target/release/minigu'
PATTERNS=EXP/'patterns/gcard/job_light_imdb_unique'
TRUTH=EXP/'patterns/truecard/job_light.csv'
RUN_ROOT=EXP/'result/gcard_query/job_light_imdb_unique_k2d5_estimate'
TMP_ROOT=EXP/'run_tmp/job_light_imdb_unique_k2d5'
DB=TMP_ROOT/'db'
GRAPH='imdb_job_light_unique_k2d5'
CARD_RE=re.compile(r",\s*([^,\s]+),\s*cardinality:\s*([0-9.eE+-]+)")
TIME_RE=re.compile(r"Time:\s*([0-9.]+)s")
PROF_RE=re.compile(r"total: (?P<total>\d+).*sample_time: (?P<sample>[0-9.]+), estimate_time: (?P<estimate>[0-9.]+), build_time: (?P<build>[0-9.]+).*cardinality: (?P<cardinality>[0-9.]+)")
def qerror(est,true):
    est=max(float(est),1.0); true=max(float(true),1.0); return max(est/true,true/est)
def parse_profiles(log):
    profiles=[]; cli=[]; cur=None
    for line in log.read_text(encoding='utf-8',errors='replace').splitlines():
        if line.startswith('[build-prof]'):
            cur={k:float(v) for k,v in re.findall(r"([a-zA-Z_+]+): ([0-9.]+)s",line)}
        elif line.startswith('total:'):
            m=PROF_RE.search(line)
            if m:
                cur=cur or {}; cur.update({'gcard_total_candidates':float(m.group('total')),'sample_time_s':float(m.group('sample')),'estimate_time_s':float(m.group('estimate')),'build_time_s':float(m.group('build'))}); profiles.append(cur); cur=None
        elif line.startswith('Time:'):
            m=TIME_RE.search(line)
            if m: cli.append(float(m.group(1)))
    return profiles,cli
def run(mode, scale_flag):
    rows=list(csv.DictReader(TRUTH.open(newline='',encoding='utf-8')))
    RUN_ROOT.mkdir(parents=True,exist_ok=True); TMP_ROOT.mkdir(parents=True,exist_ok=True)
    gql=TMP_ROOT/f'query_{mode}.gql'; log=RUN_ROOT/f'query_{mode}.log'
    lines=[f'session set graph {GRAPH}',f'call load_catalog("{GRAPH}")','call set_gcard_star_config(1, 5)']
    for r in rows: lines.append(f':time call gcard_query("{PATTERNS/(r["query"]+".json")}", 2, 500, {scale_flag}, false, 10)')
    lines.append(':quit'); gql.write_text('\n'.join(lines)+'\n',encoding='utf-8')
    with log.open('w',encoding='utf-8') as out:
        proc=subprocess.run([str(MINIGU),'execute',str(gql),'--path',str(DB)],cwd=str(ROOT),stdout=out,stderr=subprocess.STDOUT,text=True)
    if proc.returncode!=0: raise SystemExit(f'minigu exited {proc.returncode}; see {log}')
    est=[float(m.group(2)) for m in CARD_RE.finditer(log.read_text(encoding='utf-8',errors='replace'))]
    if len(est)!=len(rows): raise SystemExit(f'got {len(est)} estimates expected {len(rows)}; see {log}')
    profiles,cli=parse_profiles(log)
    out=RUN_ROOT/f'query_{mode}.csv'
    fields=['query','cardinality','time_s','true_cardinality','qerror','build_time_s','estimate_time_s','sample_time_s','cli_time_s','gcard_total_candidates']
    with out.open('w',newline='',encoding='utf-8') as f:
        w=csv.DictWriter(f,fieldnames=fields); w.writeheader()
        for i,(r,e) in enumerate(zip(rows,est)):
            prof=profiles[i] if i<len(profiles) else {}; lat=(prof.get('build_time_s',0)+prof.get('estimate_time_s',0)) if prof else (cli[i] if i<len(cli) else '')
            w.writerow({'query':r['query'],'cardinality':f'{e:.17g}','time_s':f'{lat:.9g}' if isinstance(lat,float) else lat,'true_cardinality':r['true_cardinality'],'qerror':f'{qerror(e,r["true_cardinality"]):.17g}','build_time_s':prof.get('build_time_s',''),'estimate_time_s':prof.get('estimate_time_s',''),'sample_time_s':prof.get('sample_time_s',''),'cli_time_s':f'{cli[i]:.9g}' if i<len(cli) else '','gcard_total_candidates':prof.get('gcard_total_candidates','')})
    print(mode, out, log)
def main():
    run('inner',0); run('scale',1)
if __name__=='__main__': main()
