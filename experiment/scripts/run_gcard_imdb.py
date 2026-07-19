#!/usr/bin/env python3
import argparse, csv, math, os, re, shutil, subprocess
from pathlib import Path
ROOT=Path(__file__).resolve().parents[2]
EXP=ROOT/'experiment'
MINIGU=ROOT/'target/release/minigu'
DATASET=EXP/'datasets/imdb/imdb'
PATTERNS=EXP/'patterns/gcard/imdb'
WIDE=EXP/'result/sql_baselines/imdb_q1_q28_7baselines/imdb_q1_q28_7baselines_qerror_latency.csv'
RUN_ROOT=EXP/'result/gcard_query/imdb_q1_q28_k2d5'
TMP_ROOT=EXP/'run_tmp/imdb_q1_q28_k2d5'
DB=TMP_ROOT/'db'
GRAPH='imdb'
CARD_RE=re.compile(r",\s*([^,\s]+),\s*cardinality:\s*([0-9.eE+-]+)")
TIME_RE=re.compile(r"Time:\s*([0-9.]+)s")
PROF_RE=re.compile(r"total: (?P<total>\d+).*sample_time: (?P<sample>[0-9.]+), estimate_time: (?P<estimate>[0-9.]+), build_time: (?P<build>[0-9.]+).*cardinality: (?P<cardinality>[0-9.]+)")
def qerror(est,true):
    est=max(float(est),1.0); true=max(float(true),1.0); return max(est/true,true/est)
def run_minigu(gql, log, star_len=1, star_degree=5):
    with log.open('w',encoding='utf-8') as out:
        env=os.environ.copy(); env['GCARD_MAX_STAR_LENGTH']=str(star_len); env['GCARD_MAX_STAR_DEGREE']=str(star_degree)
        p=subprocess.run([str(MINIGU),'execute',str(gql),'--path',str(DB)],cwd=str(ROOT),stdout=out,stderr=subprocess.STDOUT,text=True,env=env)
    if p.returncode!=0: raise RuntimeError(f'minigu exited {p.returncode}; see {log}')
def ensure_graph(args):
    if args.force_db and DB.exists(): shutil.rmtree(DB)
    DB.mkdir(parents=True,exist_ok=True); RUN_ROOT.mkdir(parents=True,exist_ok=True); TMP_ROOT.mkdir(parents=True,exist_ok=True)
    if (DB/f'{GRAPH}.statistic.bin').exists() and (DB/'catalog.json').exists() and not args.force_db: return
    gql=TMP_ROOT/'setup.gql'; log=RUN_ROOT/'setup.log'
    gql.write_text(f'call load_ldbc("{DATASET}", "{GRAPH}", {args.k})\n:quit\n',encoding='utf-8')
    run_minigu(gql,log,args.star_len,args.star_degree)
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
def main():
    ap=argparse.ArgumentParser(); ap.add_argument('--k',type=int,default=2); ap.add_argument('--sample-size',type=int,default=500); ap.add_argument('--tree-num',type=int,default=10); ap.add_argument('--star-len',type=int,default=1); ap.add_argument('--star-degree',type=int,default=5); ap.add_argument('--force-db',action='store_true'); args=ap.parse_args()
    ensure_graph(args)
    truth=list(csv.DictReader(WIDE.open(newline='',encoding='utf-8')))
    patterns=[PATTERNS/f"{r['query']}.json" for r in truth]
    gql=TMP_ROOT/'query.gql'; log=RUN_ROOT/'imdb.log'
    lines=[f'session set graph {GRAPH}',f'call load_catalog("{GRAPH}")',f'call set_gcard_star_config({args.star_len}, {args.star_degree})']
    for p in patterns: lines.append(f':time call gcard_query("{p}", {args.k}, {args.sample_size}, 0, false, {args.tree_num})')
    lines.append(':quit'); gql.write_text('\n'.join(lines)+'\n',encoding='utf-8')
    run_minigu(gql,log,args.star_len,args.star_degree)
    est=[float(m.group(2)) for m in CARD_RE.finditer(log.read_text(encoding='utf-8',errors='replace'))]
    if len(est)!=len(truth): raise RuntimeError(f'got {len(est)} estimates expected {len(truth)}; see {log}')
    profiles,cli=parse_profiles(log)
    out=RUN_ROOT/'detail.csv'; fields=['dataset','query','pattern','estimate','true','qerror','latency_s','build_time_s','estimate_time_s','sample_time_s','cli_time_s','gcard_total_candidates']
    with out.open('w',newline='',encoding='utf-8') as f:
        w=csv.DictWriter(f,fieldnames=fields); w.writeheader()
        for i,(r,e) in enumerate(zip(truth,est)):
            prof=profiles[i] if i<len(profiles) else {}; lat=(prof.get('build_time_s',0)+prof.get('estimate_time_s',0)+prof.get('sample_time_s',0)) if prof else (cli[i] if i<len(cli) else '')
            w.writerow({'dataset':'imdb','query':r['query'],'pattern':r['query'],'estimate':f'{e:.17g}','true':r['true'],'qerror':f'{qerror(e,r["true"]):.17g}','latency_s':f'{lat:.9g}' if isinstance(lat,float) else lat,'build_time_s':prof.get('build_time_s',''),'estimate_time_s':prof.get('estimate_time_s',''),'sample_time_s':prof.get('sample_time_s',''),'cli_time_s':f'{cli[i]:.9g}' if i<len(cli) else '','gcard_total_candidates':prof.get('gcard_total_candidates','')})
    print('detail=',out); print('log=',log)
if __name__=='__main__': main()
