# GCard Experiment Progress

Last updated: 2026-03-14

---

## Overview

GCard cardinality estimation experiments on LDBC SNB dataset.
Branch: `colin_gcard` (4 commits ahead of master)

---

## 1. System Implementation

| Module | Status | Notes |
|--------|--------|-------|
| GCard core (`gcard_query/`) | Done | query_graph, statistic, degreepiecewise, union_find, update_log |
| `create_catalog` procedure | Done | supports layer-by-layer (mode=0) and neighbor-cached (mode=1), multi-threaded |
| MATCH statement parsing | Done | commit `d84f0c8` on master |
| MATCH + Filter support | Done | commits `acd9772`, `c17250a` on `colin_gcard` |
| `mutate_graph` procedure | Done | 397 lines, supports graph mutation |
| `import_graph` procedure | Updated | significant changes (+374 lines) |

---

## 2. Dataset Preparation

| Scale Factor | CSV Data | minigu_db (Imported) | Status |
|--------------|----------|---------------------|--------|
| SF 0.1 | Ready | Ready | Done |
| SF 0.3 | Ready | Ready | Done |
| SF 1 | Ready | Ready | Done |
| SF 3 | - | - | Not prepared |
| SF 10 | - | - | Not prepared |
| SF 30 | - | - | Not prepared |
| SF 100 | - | - | Not prepared |

Data pipeline: Docker (ldbc/datagen) -> `generate.sh` -> `process.sh` -> CSV -> minigu import

---

## 3. Query Patterns (LDBC)

6 base patterns designed, each with a predicate variant = 12 total configurations.

| ID | Structure | Nodes | Edges | Cycle | Predicate Style | JSON Ready |
|----|-----------|-------|-------|-------|-----------------|------------|
| L1 | path (chain-3) | 4 | 3 | No | PA (clustered) | L1.json, L1_PA.json |
| L2 | chain | 4(?) | ? | No | PB (scattered) | L2.json, L2_PB.json |
| L3 | star | ? | ? | No | PB (scattered) | L3.json, L3_PB.json |
| L4 | cycle | 4 | 4 | Yes | PA (clustered) | L4.json, L4_PA.json |
| L5 | triangle | ? | ? | Yes | PB (scattered) | L5.json, L5_PB.json |
| L6 | cycle+branch | 5 | 6 | Yes | PA (clustered) | L6.json, L6_PA.json |

Note: The folder names (L1_path, L2_chain, etc.) differ from the README.md description (L1=chain-7, L2=cycle, etc.). The actual JSON files have been simplified/redesigned from the original LSQB queries. Verify mapping is intentional.

---

## 4. Benchmark Infrastructure

| Component | Status | Notes |
|-----------|--------|-------|
| `bench_catalog.sh` | Ready | Tests create_catalog across SF x mode x threads x repeats |
| Memory sampler | Ready | Per-second RSS tracing via `ps` |
| DuckDB baseline CLI | Ready | v0.10.2 universal binary in `experiment/duckdb` |
| `plot_catalog_bench.py` | Exists | For visualizing results |
| `bench_catalog_results.csv` | Empty | Header only, no results collected yet |
| Memory traces | Partial | Only `mem_sf0.1_m0_t1_r1.csv` exists |

---

## 5. What's Done

- [x] GCard algorithm implementation (core Rust code, ~3000+ lines)
- [x] `create_catalog` procedure with 2 build modes and parallel support
- [x] MATCH + predicate filter support in query engine
- [x] LDBC dataset generation pipeline (Docker-based)
- [x] 3 scale factors imported (SF 0.1, 0.3, 1)
- [x] 6 query patterns designed with predicate variants (12 JSON files)
- [x] Benchmark script for catalog construction performance
- [x] DuckDB baseline CLI prepared

---

## 6. What's Remaining

### High Priority
- [ ] **Run catalog benchmark** -- `bench_catalog_results.csv` is empty; execute `bench_catalog.sh` on SF 0.1/0.3/1
- [ ] **Prepare larger datasets** -- Generate and import SF 3, 10, 30, 100
- [ ] **Run GCard estimation** on L1-L6 queries and collect accuracy results
- [ ] **DuckDB baseline** -- Run equivalent queries on DuckDB for comparison

### Medium Priority
- [ ] **Reconcile pattern naming** -- README.md (L1=chain-7, L2=cycle) vs actual folders (L1_path, L2_chain); clarify the intended mapping
- [ ] **Extend benchmark** to cover cardinality estimation accuracy (not just catalog build time)
- [ ] **Add edge predicate support** in pattern JSON files (currently only L1_PA-style has predicates; some patterns lack edge predicates mentioned in README)
- [ ] **Plot results** -- Run `plot_catalog_bench.py` / `fic.ipynb` once data is collected

### Low Priority
- [ ] **Scale to SF 100** -- may require cluster resources
- [ ] **Clean up old experiment files** -- hundreds of deleted files from `experiment/patterns/` (old p1-p8, q1-q4) still in git history
- [ ] **Write up** -- compile results into paper-ready tables/figures
