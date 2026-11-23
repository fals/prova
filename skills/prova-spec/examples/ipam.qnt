// IP address management for a CNI plugin: what "correct" means, before any
// code. The implementation is free to be faster or smarter; it must never
// break these invariants. Conformance: replay ITF traces of this spec
// (quint run ipam.qnt --mbt --out-itf 'traces/ipam_{seq}.itf.json')
// against the plugin and compare `assigned` after every step.
module ipam {
  pure val PODS = Set("a", "b", "c", "d")
  pure val POOL = Set(1, 2, 3)

  // pod -> address; a pod without an address is absent from the map
  var assigned: str -> int

  pure def used(m: str -> int): Set[int] = m.keys().map(p => m.get(p))

  action init = assigned' = Map()

  // CNI ADD: a new pod gets a free address; with none free, ADD fails and
  // nothing changes (the plugin must report the error, not reuse an address).
  action add(pod: str): bool = all {
    not(assigned.keys().contains(pod)),
    val free = POOL.exclude(used(assigned))
    if (free.size() > 0)
      nondet ip = free.oneOf()
      assigned' = assigned.put(pod, ip)
    else
      assigned' = assigned,
  }

  // CNI DEL: releases the pod's address; DEL of an unknown pod is a no-op
  // (the runtime may retry DEL).
  action del(pod: str): bool =
    assigned' = if (assigned.keys().contains(pod)) assigned.keys().exclude(Set(pod)).mapBy(p => assigned.get(p)) else assigned

  action step = {
    nondet pod = PODS.oneOf()
    any { add(pod), del(pod) }
  }

  // No two pods share an address.
  val unique = assigned.keys().forall(p => assigned.keys().forall(q =>
    p != q implies assigned.get(p) != assigned.get(q)))

  // Only pool addresses are handed out.
  val inPool = used(assigned).subseteq(POOL)

  // ADD succeeds whenever an address is free (no leak): with fewer pods
  // than addresses, every pod that exists has an address or could get one.
  val noLeak = assigned.keys().size() == used(assigned).size()

  val safety = unique and inPool and noLeak

  // Exhaustion is an error, not a collision.
  run exhaustedPoolTest =
    init.then(add("a")).then(add("b")).then(add("c")).then(add("d"))
      .then(all { assert(not(assigned.keys().contains("d"))), assert(safety), assigned' = assigned })

  // DEL then ADD reuses the address safely.
  run releaseAndReuseTest =
    init.then(add("a")).then(add("b")).then(add("c")).then(del("b")).then(add("d"))
      .then(all { assert(assigned.keys().contains("d")), assert(safety), assigned' = assigned })
}
