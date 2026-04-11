#!/usr/bin/env bash
#
# Download and prepare STATS-CEB dataset for miniGU.
#
# Converts the relational STATS dataset (from stats.stackexchange.com) into
# a graph format compatible with miniGU's import pipeline.
#
# Source: https://github.com/Nathaniel-Han/End-to-End-CardEst-Benchmark
#
# Schema:
#   Vertex tables (8): users, posts, comments, badges, votes, postHistory, postLinks, tags
#   Edge tables (11):  derived from FK-PK relationships between vertex tables
#
# Usage:
#   cd experiment/dataset/stats_ceb && bash prepare.sh
#
# Output:
#   *.csv files ready for miniGU import (vertex tables + edge tables)
#   manifest.json for miniGU import_graph procedure

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
cd "$SCRIPT_DIR"

RAW_DIR="$SCRIPT_DIR/raw"
mkdir -p "$RAW_DIR"

log() { echo "[$(date '+%H:%M:%S')] $*"; }

# ── Step 1: Download raw data ────────────────────────────────────────────
REPO_URL="https://raw.githubusercontent.com/Nathaniel-Han/End-to-End-CardEst-Benchmark/master/datasets/stats_simplified"
TABLES=(badges comments postHistory postLinks posts tags users votes)

log "Downloading STATS-CEB dataset..."
for t in "${TABLES[@]}"; do
    if [[ ! -f "$RAW_DIR/${t}.csv" ]]; then
        log "  Downloading ${t}.csv..."
        curl -sL "${REPO_URL}/${t}.csv" -o "$RAW_DIR/${t}.csv"
    else
        log "  ${t}.csv already exists, skip"
    fi
done

# ── Step 2: Convert to miniGU graph format ───────────────────────────────
log "Converting to miniGU graph format..."
python3 - "$RAW_DIR" "$SCRIPT_DIR" <<'PYEOF'
import csv
import json
import os
import sys

raw_dir = sys.argv[1]
out_dir = sys.argv[2]

# ── Vertex tables: rename "Id" to "id", keep all attribute columns ──
vertex_tables = {
    "users":       {"pk": "Id", "attrs": ["Reputation", "CreationDate", "Views", "UpVotes", "DownVotes"]},
    "posts":       {"pk": "Id", "attrs": ["PostTypeId", "CreationDate", "Score", "ViewCount", "OwnerUserId",
                                           "AnswerCount", "CommentCount", "FavoriteCount", "LastEditorUserId"]},
    "comments":    {"pk": "Id", "attrs": ["PostId", "Score", "CreationDate", "UserId"]},
    "badges":      {"pk": "Id", "attrs": ["UserId", "Date"]},
    "votes":       {"pk": "Id", "attrs": ["PostId", "VoteTypeId", "CreationDate", "UserId", "BountyAmount"]},
    "postHistory": {"pk": "Id", "attrs": ["PostHistoryTypeId", "PostId", "CreationDate", "UserId"]},
    "postLinks":   {"pk": "Id", "attrs": ["CreationDate", "PostId", "RelatedPostId", "LinkTypeId"]},
    "tags":        {"pk": "Id", "attrs": ["Count", "ExcerptPostId"]},
}

# ── Edge definitions: (edge_label, src_table, src_fk_col, dst_table) ──
# Each edge represents a FK → PK(Id) relationship
edge_defs = [
    ("comments_postid_posts",       "comments",    "PostId",          "posts"),
    ("comments_userid_users",       "comments",    "UserId",          "users"),
    ("badges_userid_users",         "badges",      "UserId",          "users"),
    ("tags_excerptpostid_posts",    "tags",        "ExcerptPostId",   "posts"),
    ("postlinks_postid_posts",      "postLinks",   "PostId",          "posts"),
    ("postlinks_relatedpostid_posts","postLinks",  "RelatedPostId",   "posts"),
    ("posthistory_postid_posts",    "postHistory",  "PostId",         "posts"),
    ("posthistory_userid_users",    "postHistory",  "UserId",         "users"),
    ("votes_postid_posts",          "votes",       "PostId",          "posts"),
    ("votes_userid_users",          "votes",       "UserId",          "users"),
    ("posts_owneruserid_users",     "posts",       "OwnerUserId",     "users"),
]

# VertexId in miniGU is u64 (unsigned), so negative IDs must be remapped.
# We remap per-table: for each table, collect all IDs (PKs) and FK references,
# and convert any negative ID to (MAX_POSITIVE_ID_FOR_TABLE + offset).
NEGATIVE_ID_OFFSET = 2_000_000_000  # -1 -> 2000000001, -2 -> 2000000002, etc.

def remap_id(val):
    """Remap a negative integer ID string to a positive one."""
    if not val or val == "" or val == "NA":
        return val
    try:
        n = int(val)
        if n < 0:
            return str(NEGATIVE_ID_OFFSET - n)  # -1 -> 2000000001
        return val
    except ValueError:
        return val

# Read raw CSV data into memory (keyed by table name)
raw_data = {}
for tname in vertex_tables:
    csv_path = os.path.join(raw_dir, f"{tname}.csv")
    if not os.path.exists(csv_path):
        print(f"  WARNING: {csv_path} not found, skipping")
        continue
    with open(csv_path, newline="") as f:
        reader = csv.DictReader(f)
        rows = list(reader)
    raw_data[tname] = rows
    print(f"  Loaded {tname}: {len(rows)} rows")

# Write vertex CSVs: id + attribute columns
for tname, info in vertex_tables.items():
    if tname not in raw_data:
        continue
    rows = raw_data[tname]
    pk = info["pk"]
    attrs = info["attrs"]

    # Only include attributes that exist in the actual CSV
    available_attrs = [a for a in attrs if a in rows[0]] if rows else []
    header = ["id"] + [a.lower() for a in available_attrs]

    out_path = os.path.join(out_dir, f"{tname.lower()}.csv")
    with open(out_path, "w", newline="") as f:
        writer = csv.writer(f)
        writer.writerow(header)
        for row in rows:
            pk_val = row.get(pk, "")
            if not pk_val:
                continue
            pk_val = remap_id(pk_val)
            vals = [pk_val] + [row.get(a, "") for a in available_attrs]
            writer.writerow(vals)
    print(f"  Wrote vertex table: {out_path} ({len(rows)} rows, cols={header})")

# Write edge CSVs: src,dst (+ optional attributes from the source row)
for edge_label, src_table, fk_col, dst_table in edge_defs:
    if src_table not in raw_data:
        continue
    rows = raw_data[src_table]
    pk = vertex_tables[src_table]["pk"]

    out_path = os.path.join(out_dir, f"{edge_label}.csv")
    edge_count = 0
    with open(out_path, "w", newline="") as f:
        writer = csv.writer(f)
        writer.writerow(["src", "dst"])
        for row in rows:
            src_val = row.get(pk, "")
            dst_val = row.get(fk_col, "")
            # Skip rows with empty FK (NULL foreign key)
            if not src_val or not dst_val or dst_val == "" or dst_val == "NA":
                continue
            src_val = remap_id(src_val)
            dst_val = remap_id(dst_val)
            writer.writerow([src_val, dst_val])
            edge_count += 1
    print(f"  Wrote edge table: {edge_label} ({edge_count} edges)")

# ── manifest.json is maintained manually (correct miniGU format) ──
# See manifest.json in this directory.
print(f"\n  Note: manifest.json is maintained manually, not regenerated.")

# ── Summary ──
print(f"\n=== STATS-CEB Dataset Ready ===")
print(f"  Vertex tables: {len(vertex_tables)}")
print(f"  Edge tables:   {len(edge_defs)}")
print(f"  Location:      {out_dir}")
PYEOF

log "Done. Dataset ready at: $SCRIPT_DIR"
log ""
log "To import into miniGU:"
log "  minigu> call import_graph(\"$SCRIPT_DIR/manifest.json\")"
log "  minigu> call gcard_build(\"stats_ceb\", 2, 500)"
