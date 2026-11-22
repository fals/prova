// Shortest paths on a small weighted graph: the reference answer any
// implementation (BFS, Dijkstra, A*, a clever cache) must reproduce. The
// objective of an experiment is then the cost of getting there (nodes
// expanded, time), never the answer, which is fixed here.
module shortest {
  pure val NODES = 0.to(5)
  pure val INF = 1000
  // (from, to) -> weight; directed
  pure val EDGES: (int, int) -> int = Map(
    (0, 1) -> 4, (0, 2) -> 1, (2, 1) -> 2, (1, 3) -> 1,
    (2, 3) -> 5, (3, 4) -> 3, (4, 5) -> 1, (2, 5) -> 12,
  )

  pure def w(u: int, v: int): int = if (EDGES.keys().contains((u, v))) EDGES.get((u, v)) else INF

  pure def min(a: int, b: int): int = if (a < b) a else b

  // Bellman-Ford: |NODES| - 1 relaxation rounds from `src`.
  pure def relax(d: int -> int): int -> int =
    NODES.mapBy(v => NODES.fold(d.get(v), (best, u) =>
      if (d.get(u) < INF and w(u, v) < INF) min(best, d.get(u) + w(u, v)) else best))

  pure def dist(src: int): int -> int = {
    val start = NODES.mapBy(n => if (n == src) 0 else INF)
    range(1, NODES.size()).foldl(start, (d, i) => relax(d))
  }

  // A path is a list of nodes; its cost, INF if an edge is missing.
  pure def cost(path: List[int]): int =
    if (path.length() < 2) 0
    else range(0, path.length() - 1).foldl(0, (c, i) =>
      if (c >= INF or w(path[i], path[i + 1]) >= INF) INF else c + w(path[i], path[i + 1]))

  // An answer for (src, dst) is correct iff it starts at src, ends at dst,
  // uses only edges, and costs exactly dist(src)(dst).
  pure def correct(src: int, dst: int, path: List[int]): bool = and {
    path.length() > 0,
    path[0] == src,
    path[path.length() - 1] == dst,
    cost(path) == dist(src).get(dst),
  }

  // Reference values the conformance driver compares against.
  run distancesTest = all {
    assert(dist(0) == Map(0 -> 0, 1 -> 3, 2 -> 1, 3 -> 4, 4 -> 7, 5 -> 8)),
    assert(dist(5).get(0) == INF),
  }

  run correctnessTest = all {
    assert(correct(0, 5, [0, 2, 1, 3, 4, 5])),
    assert(not(correct(0, 5, [0, 2, 5]))),       // valid path, not shortest
    assert(not(correct(0, 3, [0, 3]))),          // no such edge
  }

  // No state: the module is a reference model, checked by its tests.
  var unused: int
  action init = unused' = 0
  action step = unused' = unused
}
