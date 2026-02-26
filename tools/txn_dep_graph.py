#!/usr/bin/env python3
"""
Transaction Dependency Graph Analyzer for Turso MVCC logs.

Inspired by Elle (Kingsbury) / Adya's formalism. Builds a serialization
dependency graph from MVCC trace logs and searches for cycles that
indicate isolation anomalies.

The three dependency edge types (between committed transactions):

  WW: T_i installed version v, T_j installed the next version v+1.
      T_i --ww--> T_j  (consecutive in the version chain)

  WR: T_i installed version v, T_j read version v.
      T_i --wr--> T_j  (T_j observed T_i's write)

  RW: T_i read version v, T_j installed version v+1.
      T_i --rw--> T_j  (anti-dependency: T_i saw state before T_j's write)

Version visibility is determined by wall-clock timestamps from the log:
  T_j sees the latest committed version whose commit wall-clock time
  is before T_j's begin wall-clock time.

Cycles indicate anomalies:
  WW-only cycle:                    G0  (dirty write)
  Cycle w/ WR (no RW):             G1c (circular information flow)
  Cycle w/ exactly 1 RW edge:      G-single (read skew)
  Cycle w/ 2+ RW edges:            G2-item (write skew)
  RW+WW 2-cycle on same item:      P4  (lost update)

Usage:
  python3 tools/txn_dep_graph.py <logfile> [options]
"""

import re
import sys
import argparse
from collections import defaultdict
from dataclasses import dataclass, field
from enum import Enum, auto
from typing import Optional
from datetime import datetime


# ── ANSI stripping ─────────────────────────────────────────────────────
ANSI_RE = re.compile(r'\x1b\[[0-9;]*m')

# ── Log line patterns ──────────────────────────────────────────────────

TS_RE = re.compile(r'^(\d{4}-\d{2}-\d{2}T\d{2}:\d{2}:\d{2}\.\d+)Z')

SET_MV_RE = re.compile(r'set_mv_tx: Some\(\((\d+), (Concurrent|Write)\)\)')
# Supports both old format "begin_tx(tx_id=N)" and new "begin_tx(tx_id=N, begin_ts=M)"
BEGIN_TX_RE = re.compile(r'mvcc::database.*begin_tx\(tx_id=(\d+)(?:, begin_ts=(\d+))?\)')

READ_RE = re.compile(
    r'mvcc::database.*read\(tx_id=(\d+), id=RowID \{ table_id: MVTableId\((-?\d+)\), row_id: Int\((\d+)\) \}\)')

INSERT_RE = re.compile(
    r'mvcc::database.*insert\(tx_id=(\d+), row\.id=RowID \{ table_id: MVTableId\((-?\d+)\), row_id: Int\((\d+)\) \}\)')

VERSION_RE = re.compile(
    r'record_created_table_version\(tx_id=(\d+), table_id=MVTableId\((-?\d+)\), row_id=(\d+), version_id=(\d+)\)')

ABORT_RE = re.compile(r'mvcc::database.*abort\(tx_id=(\d+)\)')

PREPARE_RE = re.compile(r'prepare_tx\(tx_id=(\d+), end_ts=(\d+)\)')

LOGGED_RE = re.compile(r'logged\(tx_id=(\d+), end_ts=(\d+)\)')


# ── Data structures ───────────────────────────────────────────────────

class TxOutcome(Enum):
    PENDING = auto()
    COMMITTED = auto()
    ABORTED = auto()


@dataclass
class TxInfo:
    tx_id: int
    tx_type: str = "Concurrent"
    outcome: TxOutcome = TxOutcome.PENDING
    begin_ts: Optional[int] = None  # MVCC logical begin timestamp
    end_ts: Optional[int] = None
    # Wall-clock timestamps (seconds as float) — fallback only
    begin_wallclock: Optional[float] = None
    commit_wallclock: Optional[float] = None
    read_set: set = field(default_factory=set)
    write_set: set = field(default_factory=set)
    versions_written: dict = field(default_factory=dict)


class DepType(Enum):
    WW = "ww"
    WR = "wr"
    RW = "rw"


@dataclass(frozen=True)
class Edge:
    src: int
    dst: int
    dep: DepType
    item: tuple

    def __repr__(self):
        return f"T{self.src} --{self.dep.value}({self.item})--> T{self.dst}"


# ── Parsing ────────────────────────────────────────────────────────────

def parse_wallclock(s: str) -> float:
    """Parse ISO timestamp to seconds (float)."""
    dt = datetime.fromisoformat(s)
    return dt.timestamp()


def parse_log(logfile: str) -> tuple[dict[int, TxInfo], dict]:
    txns: dict[int, TxInfo] = {}
    version_chain: dict[tuple, list] = defaultdict(list)

    with open(logfile) as f:
        for line in f:
            line = ANSI_RE.sub('', line)

            # Extract wall-clock timestamp
            ts_match = TS_RE.match(line)
            wallclock = parse_wallclock(ts_match.group(1)) if ts_match else None

            m = SET_MV_RE.search(line)
            if m:
                tx_id, tx_type = int(m.group(1)), m.group(2)
                if tx_id not in txns:
                    txns[tx_id] = TxInfo(tx_id=tx_id, tx_type=tx_type)
                continue

            m = BEGIN_TX_RE.search(line)
            if m:
                tx_id = int(m.group(1))
                if tx_id not in txns:
                    txns[tx_id] = TxInfo(tx_id=tx_id)
                if m.group(2):
                    txns[tx_id].begin_ts = int(m.group(2))
                if wallclock and txns[tx_id].begin_wallclock is None:
                    txns[tx_id].begin_wallclock = wallclock
                continue

            m = READ_RE.search(line)
            if m:
                tx_id = int(m.group(1))
                item = (int(m.group(2)), int(m.group(3)))
                if tx_id not in txns:
                    txns[tx_id] = TxInfo(tx_id=tx_id)
                txns[tx_id].read_set.add(item)
                continue

            m = INSERT_RE.search(line)
            if m:
                tx_id = int(m.group(1))
                item = (int(m.group(2)), int(m.group(3)))
                if tx_id not in txns:
                    txns[tx_id] = TxInfo(tx_id=tx_id)
                txns[tx_id].write_set.add(item)
                continue

            m = VERSION_RE.search(line)
            if m:
                tx_id = int(m.group(1))
                item = (int(m.group(2)), int(m.group(3)))
                ver = int(m.group(4))
                if tx_id not in txns:
                    txns[tx_id] = TxInfo(tx_id=tx_id)
                txns[tx_id].versions_written[item] = ver
                version_chain[item].append((ver, tx_id))
                continue

            m = ABORT_RE.search(line)
            if m:
                tx_id = int(m.group(1))
                if tx_id in txns:
                    txns[tx_id].outcome = TxOutcome.ABORTED
                continue

            m = PREPARE_RE.search(line)
            if m:
                tx_id, end_ts = int(m.group(1)), int(m.group(2))
                if tx_id in txns:
                    txns[tx_id].end_ts = end_ts
                continue

            m = LOGGED_RE.search(line)
            if m:
                tx_id, end_ts = int(m.group(1)), int(m.group(2))
                if tx_id in txns:
                    txns[tx_id].outcome = TxOutcome.COMMITTED
                    txns[tx_id].end_ts = end_ts
                    if wallclock:
                        txns[tx_id].commit_wallclock = wallclock
                continue

    for item in version_chain:
        version_chain[item].sort(key=lambda x: x[0])

    return txns, version_chain


# ── Graph construction ─────────────────────────────────────────────────

def determine_read_version(
    reader: TxInfo,
    committed_chain: list[tuple[int, int]],  # [(version_id, tx_id)]
    committed: dict[int, TxInfo],
) -> Optional[tuple[int, int]]:
    """
    Determine which version a reader saw.

    Prefers MVCC logical timestamps (begin_ts/end_ts) when available.
    Falls back to wall-clock timestamps from log lines.

    Under snapshot isolation, reader sees the latest version whose
    writer's end_ts < reader's begin_ts.

    Returns (version_id, writer_tx_id) or None.
    """
    # Try MVCC logical timestamps first
    if reader.begin_ts is not None:
        best = None
        for ver, w_tid in committed_chain:
            w_tx = committed.get(w_tid)
            if w_tx is None or w_tx.end_ts is None:
                continue
            if w_tx.end_ts < reader.begin_ts:
                best = (ver, w_tid)
        if best is not None:
            return best
        # begin_ts was available but no writer qualified — reader sees
        # initial state (before any writes). Return None.
        return None

    # Fallback: wall-clock timestamps
    if reader.begin_wallclock is not None:
        best = None
        for ver, w_tid in committed_chain:
            w_tx = committed.get(w_tid)
            if w_tx is None or w_tx.commit_wallclock is None:
                continue
            if w_tx.commit_wallclock < reader.begin_wallclock:
                best = (ver, w_tid)
        return best

    return None


def build_dependency_graph(
    txns: dict[int, TxInfo],
    version_chain: dict,
    filter_table: Optional[int] = None,
) -> tuple[list[Edge], dict]:
    """
    Build Elle-style serialization dependency graph.

    Returns (edges, read_versions) where read_versions maps
    (tx_id, item) -> (version_id, writer_tx_id) for diagnostics.
    """
    committed = {tid: t for tid, t in txns.items()
                 if t.outcome == TxOutcome.COMMITTED}

    committed_chains: dict[tuple, list] = {}
    for item, chain in version_chain.items():
        if filter_table is not None and item[0] != filter_table:
            continue
        cc = [(v, tid) for v, tid in chain if tid in committed]
        if cc:
            committed_chains[item] = cc

    edges: list[Edge] = []
    seen: set = set()
    read_versions: dict = {}  # (tx_id, item) -> (ver, writer_tid)

    for item, chain in committed_chains.items():
        ver_to_idx = {v: i for i, (v, _) in enumerate(chain)}

        # ── WW edges: consecutive writers ──
        for i in range(len(chain) - 1):
            _, ti = chain[i]
            _, tj = chain[i + 1]
            if ti != tj:
                key = (ti, tj, DepType.WW)
                if key not in seen:
                    seen.add(key)
                    edges.append(Edge(ti, tj, DepType.WW, item))

        # ── WR and RW edges based on version visibility ──
        for r_tid, r_tx in committed.items():
            if item not in r_tx.read_set:
                continue

            rv = determine_read_version(r_tx, chain, committed)
            if rv is None:
                continue
            read_ver, read_writer = rv
            read_versions[(r_tid, item)] = rv

            # WR: writer_of(read_ver) --wr--> reader
            if read_writer != r_tid:
                key = (read_writer, r_tid, DepType.WR)
                if key not in seen:
                    seen.add(key)
                    edges.append(Edge(read_writer, r_tid, DepType.WR, item))

            # RW: reader --rw--> writer_of(read_ver + 1)
            idx = ver_to_idx.get(read_ver)
            if idx is not None and idx + 1 < len(chain):
                _, next_writer = chain[idx + 1]
                if next_writer != r_tid:
                    key = (r_tid, next_writer, DepType.RW)
                    if key not in seen:
                        seen.add(key)
                        edges.append(Edge(r_tid, next_writer, DepType.RW, item))

    return edges, read_versions


# ── Cycle detection ───────────────────────────────────────────────────

def tarjan_scc(adj, nodes):
    idx = [0]
    stack, on_stack = [], set()
    index, lowlink = {}, {}
    sccs = []

    def sc(v):
        index[v] = lowlink[v] = idx[0]
        idx[0] += 1
        stack.append(v)
        on_stack.add(v)
        for w in adj.get(v, set()):
            if w not in index:
                sc(w)
                lowlink[v] = min(lowlink[v], lowlink[w])
            elif w in on_stack:
                lowlink[v] = min(lowlink[v], index[w])
        if lowlink[v] == index[v]:
            scc = []
            while True:
                w = stack.pop()
                on_stack.remove(w)
                scc.append(w)
                if w == v:
                    break
            if len(scc) > 1:
                sccs.append(scc)

    sys.setrecursionlimit(100000)
    for v in sorted(nodes):
        if v not in index:
            sc(v)
    return sccs


def find_cycles(edges):
    adj = defaultdict(set)
    edge_map = defaultdict(list)
    nodes = set()
    for e in edges:
        adj[e.src].add(e.dst)
        edge_map[(e.src, e.dst)].append(e)
        nodes.add(e.src)
        nodes.add(e.dst)

    sccs = tarjan_scc(adj, nodes)
    cycles = []

    for scc in sccs:
        scc_set = set(scc)
        found = set()

        # 2-cycles
        for a in scc:
            for b in adj.get(a, set()):
                if b in scc_set and b > a and a in adj.get(b, set()):
                    for e1 in edge_map[(a, b)]:
                        for e2 in edge_map[(b, a)]:
                            sig = frozenset([(e1.src, e1.dst, e1.dep, e1.item),
                                             (e2.src, e2.dst, e2.dep, e2.item)])
                            if sig not in found:
                                found.add(sig)
                                cycles.append([e1, e2])

        # 3-cycles
        adj_scc = {n: [x for x in adj.get(n, set()) if x in scc_set] for n in scc}
        for a in scc:
            for b in adj_scc.get(a, []):
                if b == a:
                    continue
                for c in adj_scc.get(b, []):
                    if c in (a, b):
                        continue
                    if a in adj_scc.get(c, []):
                        e1 = edge_map[(a, b)][0]
                        e2 = edge_map[(b, c)][0]
                        e3 = edge_map[(c, a)][0]
                        sig = frozenset([(e1.src, e1.dst, e1.dep, e1.item),
                                         (e2.src, e2.dst, e2.dep, e2.item),
                                         (e3.src, e3.dst, e3.dep, e3.item)])
                        if sig not in found:
                            found.add(sig)
                            cycles.append([e1, e2, e3])

    return cycles


# ── Anomaly classification ────────────────────────────────────────────

def classify_anomaly(cycle):
    deps = [e.dep for e in cycle]
    dep_set = set(deps)
    rw_count = deps.count(DepType.RW)
    n = len(cycle)

    if dep_set == {DepType.WW}:
        return "G0 (dirty write)"
    if dep_set <= {DepType.WW, DepType.WR} and DepType.WR in dep_set:
        return "G1c (circular information flow)"
    if DepType.RW in dep_set:
        if n == 2 and dep_set == {DepType.RW, DepType.WW}:
            return "P4 (lost update)"
        if n == 2 and dep_set == {DepType.RW, DepType.WR}:
            return "G-single (read skew, 2-cycle)"
        if rw_count == 1:
            return "G-single (read skew)"
        if rw_count >= 2:
            return "G2-item (write skew)"
    return f"cycle ({'+'.join(d.value for d in dep_set)})"


# ── Output ─────────────────────────────────────────────────────────────

def item_str(item):
    return f"table={item[0]},row={item[1]}"


def print_summary(txns, version_chain, edges, cycles, read_versions, filter_table):
    committed = [t for t in txns.values() if t.outcome == TxOutcome.COMMITTED]
    aborted = [t for t in txns.values() if t.outcome == TxOutcome.ABORTED]

    print("=" * 72)
    print("TRANSACTION DEPENDENCY GRAPH ANALYSIS (Elle-style)")
    print("=" * 72)
    print(f"Transactions:  {len(txns)} total, {len(committed)} committed, "
          f"{len(aborted)} aborted")
    if filter_table is not None:
        print(f"Filter:        table_id={filter_table}")
    print()

    # Version chain
    print("Version chains:")
    for item in sorted(version_chain.keys()):
        if filter_table is not None and item[0] != filter_table:
            continue
        chain = version_chain[item]
        cc = [(v, tid) for v, tid in chain if tid in txns
              and txns[tid].outcome == TxOutcome.COMMITTED]
        print(f"  ({item_str(item)}): {len(cc)} committed versions")
        if len(cc) <= 20:
            for v, tid in cc:
                print(f"    v{v} <- T{tid} (end_ts={txns[tid].end_ts})")
        else:
            for v, tid in cc[:5]:
                print(f"    v{v} <- T{tid} (end_ts={txns[tid].end_ts})")
            print(f"    ... ({len(cc) - 10} more) ...")
            for v, tid in cc[-5:]:
                print(f"    v{v} <- T{tid} (end_ts={txns[tid].end_ts})")
    print()

    # Read version analysis (critical for lost update detection)
    print("Read version analysis (what each committed writer read):")
    mismatches = []
    for item in sorted(version_chain.keys()):
        if filter_table is not None and item[0] != filter_table:
            continue
        cc = [(v, tid) for v, tid in version_chain[item]
              if tid in txns and txns[tid].outcome == TxOutcome.COMMITTED]
        for i, (ver, tid) in enumerate(cc):
            rv = read_versions.get((tid, item))
            if rv is None:
                continue
            read_ver, read_writer = rv
            # Expected: writer of v_{k} should have read v_{k-1}
            expected_ver = cc[i - 1][0] if i > 0 else None
            ok = (expected_ver is None) or (read_ver == expected_ver)
            if not ok:
                mismatches.append((tid, ver, read_ver, expected_ver, read_writer))
                print(f"  !! T{tid} wrote v{ver} but read v{read_ver} "
                      f"(expected v{expected_ver}) "
                      f"-- STALE READ, lost update!")
            elif expected_ver is not None:
                # Only print first/last few in long runs
                pass

    if not mismatches:
        print("  All committed writers read the immediately preceding version.")
        print("  No stale reads detected.")
    else:
        print(f"\n  FOUND {len(mismatches)} STALE READS (potential lost updates)")
    print()

    # Edge summary
    ww = [e for e in edges if e.dep == DepType.WW]
    wr = [e for e in edges if e.dep == DepType.WR]
    rw = [e for e in edges if e.dep == DepType.RW]
    print(f"Dependency edges: {len(edges)} total")
    print(f"  WW: {len(ww)}  WR: {len(wr)}  RW: {len(rw)}")
    print()

    # Cycles
    if not cycles:
        print("NO CYCLES FOUND")
        print("Serialization graph is acyclic — consistent with correct SI.")
    else:
        seen_sigs = set()
        unique = []
        for c in cycles:
            sig = frozenset((e.src, e.dst, e.dep.value, e.item) for e in c)
            if sig not in seen_sigs:
                seen_sigs.add(sig)
                unique.append(c)

        by_type = defaultdict(list)
        for c in unique:
            by_type[classify_anomaly(c)].append(c)

        print(f"ANOMALY CYCLES: {len(unique)} unique")
        print("=" * 72)
        for atype, cs in sorted(by_type.items()):
            print(f"\n  {atype}: {len(cs)} cycle(s)")
            for c in cs[:5]:
                print(f"    Cycle:")
                for e in c:
                    src_ver = txns[e.src].versions_written.get(e.item, '?')
                    dst_ver = txns[e.dst].versions_written.get(e.item, '?')
                    rv_src = read_versions.get((e.src, e.item))
                    rv_dst = read_versions.get((e.dst, e.item))
                    src_read = f"read v{rv_src[0]}" if rv_src else "no read"
                    dst_read = f"read v{rv_dst[0]}" if rv_dst else "no read"
                    print(f"      {e}")
                    print(f"        T{e.src}: wrote v{src_ver}, {src_read}")
                    print(f"        T{e.dst}: wrote v{dst_ver}, {dst_read}")
            if len(cs) > 5:
                print(f"    ... and {len(cs) - 5} more")
    print()


def print_verbose(txns, edges, read_versions, filter_table):
    print("\n" + "=" * 72)
    print("PER-TRANSACTION DETAIL (committed only)")
    print("=" * 72)
    for tx_id in sorted(txns.keys()):
        tx = txns[tx_id]
        if tx.outcome != TxOutcome.COMMITTED:
            continue
        rset = {i for i in tx.read_set if filter_table is None or i[0] == filter_table}
        wset = {i for i in tx.write_set if filter_table is None or i[0] == filter_table}
        if not rset and not wset:
            continue
        print(f"\nT{tx_id} end_ts={tx.end_ts} type={tx.tx_type}")
        print(f"  begin={tx.begin_wallclock} commit={tx.commit_wallclock}")
        for item in sorted(rset | wset):
            rv = read_versions.get((tx_id, item))
            wv = tx.versions_written.get(item)
            parts = []
            if item in rset:
                parts.append(f"read v{rv[0]} (from T{rv[1]})" if rv else "read ?")
            if wv:
                parts.append(f"wrote v{wv}")
            print(f"  ({item_str(item)}): {', '.join(parts)}")
        out = [e for e in edges if e.src == tx_id]
        inc = [e for e in edges if e.dst == tx_id]
        if out:
            print(f"  -> {[str(e) for e in out]}")
        if inc:
            print(f"  <- {[str(e) for e in inc]}")


def print_rounds(txns, filter_table):
    concurrent = sorted(
        [t for t in txns.values() if t.tx_type == "Concurrent"],
        key=lambda t: t.tx_id
    )
    rounds, cur = [], []
    for tx in concurrent:
        cur.append(tx)
        if len(cur) == 16:
            rounds.append(cur)
            cur = []
    if cur:
        rounds.append(cur)

    print("\n" + "=" * 72)
    print("PER-ROUND ANALYSIS")
    print("=" * 72)
    for i, rnd in enumerate(rounds):
        c = sum(1 for t in rnd if t.outcome == TxOutcome.COMMITTED)
        winners = [t.tx_id for t in rnd if t.outcome == TxOutcome.COMMITTED]
        print(f"  Round {i+1:>3}: ids={rnd[0].tx_id}..{rnd[-1].tx_id}  "
              f"committed={c}  winners={winners}")


def print_dot(txns, edges, cycles):
    cycle_edges = set()
    for c in cycles:
        for e in c:
            cycle_edges.add((e.src, e.dst, e.dep))

    print("digraph txn_deps {")
    print("  rankdir=LR;")
    print('  node [shape=box fontname="Courier"];')
    print('  edge [fontname="Courier"];')

    committed = sorted(tid for tid, t in txns.items()
                       if t.outcome == TxOutcome.COMMITTED)
    for tid in committed:
        t = txns[tid]
        in_cycle = any((tid == e.src or tid == e.dst) for c in cycles for e in c)
        color = "red" if in_cycle else "black"
        vers = ','.join(f"v{v}" for v in t.versions_written.values())
        print(f'  T{tid} [label="T{tid}\\nts={t.end_ts}\\n{vers}" '
              f'color="{color}" {"penwidth=2" if in_cycle else ""}];')

    colors = {DepType.WW: "red", DepType.WR: "blue", DepType.RW: "orange"}
    for e in edges:
        bold = (e.src, e.dst, e.dep) in cycle_edges
        print(f'  T{e.src} -> T{e.dst} [label="{e.dep.value}" '
              f'color="{colors[e.dep]}" '
              f'{"penwidth=2 style=bold " if bold else ""}];')

    print("}")


# ── Main ───────────────────────────────────────────────────────────────

def main():
    p = argparse.ArgumentParser(
        description="Elle-style transaction dependency graph analyzer")
    p.add_argument("logfile")
    p.add_argument("--filter-table", type=int, default=None)
    p.add_argument("--dot", action="store_true")
    p.add_argument("--verbose", action="store_true")
    p.add_argument("--rounds", action="store_true")

    args = p.parse_args()

    txns, version_chain = parse_log(args.logfile)
    edges, read_versions = build_dependency_graph(
        txns, version_chain, filter_table=args.filter_table)
    cycles = find_cycles(edges)

    if args.dot:
        print_dot(txns, edges, cycles)
    else:
        print_summary(txns, version_chain, edges, cycles, read_versions,
                      args.filter_table)
        if args.rounds:
            print_rounds(txns, args.filter_table)
        if args.verbose:
            print_verbose(txns, edges, read_versions, args.filter_table)


if __name__ == "__main__":
    main()
