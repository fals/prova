// Experiment comparison lifecycle (ADR 0007, 0011).
//
// One comparison measures a baseline and a candidate under a protocol that
// freezes at the first trial. A verdict needs the minimum trials on both
// arms, and is only valid when all trials ran on the same machine
// fingerprint. Trials are append-only.
module experiment {
  type Arm = Baseline | Candidate
  type Verdict = NoVerdict | Accept | Reject | Inconclusive | Invalid

  pure val MIN_TRIALS = 2
  pure val MAX_TRIALS = 4
  pure val FINGERPRINTS = Set(1, 2)

  var protocol: int
  var frozenProtocol: int
  var baseTrials: int
  var candTrials: int
  var fingerprints: Set[int]
  var verdict: Verdict

  action init = all {
    protocol' = 0,
    frozenProtocol' = 0,
    baseTrials' = 0,
    candTrials' = 0,
    fingerprints' = Set(),
    verdict' = NoVerdict,
  }

  pure def total(b: int, c: int): int = b + c

  // The protocol may change only before the first trial.
  action setProtocol(v: int): bool = all {
    total(baseTrials, candTrials) == 0,
    protocol' = v,
    frozenProtocol' = v,
    baseTrials' = baseTrials, candTrials' = candTrials,
    fingerprints' = fingerprints, verdict' = verdict,
  }

  // Trials alternate arms (interleaving); the next arm is the one behind.
  action trial(fp: int): bool = all {
    verdict == NoVerdict or verdict == Inconclusive,
    total(baseTrials, candTrials) < 2 * MAX_TRIALS,
    fingerprints' = fingerprints.union(Set(fp)),
    if (baseTrials <= candTrials)
      all { baseTrials' = baseTrials + 1, candTrials' = candTrials }
    else
      all { baseTrials' = baseTrials, candTrials' = candTrials + 1 },
    verdict' = NoVerdict,
    protocol' = protocol, frozenProtocol' = frozenProtocol,
  }

  // Decide. The statistics are abstracted as a nondeterministic outcome;
  // what the spec fixes is when a decision is allowed and which one.
  action decide(outcome: Verdict): bool = all {
    verdict == NoVerdict,
    baseTrials >= MIN_TRIALS,
    candTrials >= MIN_TRIALS,
    outcome != NoVerdict,
    outcome != Invalid,
    verdict' = if (fingerprints.size() > 1) Invalid
               // Inconclusive at the maximum cannot ask for more trials.
               else if (outcome == Inconclusive and candTrials >= MAX_TRIALS) Reject
               else outcome,
    protocol' = protocol, frozenProtocol' = frozenProtocol,
    baseTrials' = baseTrials, candTrials' = candTrials, fingerprints' = fingerprints,
  }

  action step = {
    nondet v = 1.to(3).oneOf()
    nondet fp = FINGERPRINTS.oneOf()
    nondet o = Set(Accept, Reject, Inconclusive).oneOf()
    any { setProtocol(v), trial(fp), decide(o) }
  }

  // ---------------- invariants

  val verdictNeedsEvidence =
    (verdict != NoVerdict) implies (baseTrials >= MIN_TRIALS and candTrials >= MIN_TRIALS)

  val protocolFrozen =
    total(baseTrials, candTrials) > 0 implies protocol == frozenProtocol

  val armsInterleaved =
    baseTrials - candTrials <= 1 and candTrials <= baseTrials

  val mixedMachinesAreInvalid =
    (verdict != NoVerdict and fingerprints.size() > 1) implies verdict == Invalid

  val boundedTrials = baseTrials <= MAX_TRIALS and candTrials <= MAX_TRIALS

  val safety = all {
    verdictNeedsEvidence,
    protocolFrozen,
    armsInterleaved,
    mixedMachinesAreInvalid,
    boundedTrials,
  }

  // Regression (ADR 0011 example 2): peeking before the minimum is refused.
  run peekingIsRefusedTest =
    init.then(setProtocol(2)).then(trial(1)).then(trial(1)).then(trial(1)).then(decide(Accept)).fail()

  // Regression: the protocol cannot move after the first trial.
  run goalpostsCannotMoveTest =
    init.then(setProtocol(2)).then(trial(1)).then(setProtocol(3)).fail()

  // Trials from two machines make any verdict invalid.
  run mixedMachinesTest =
    init
      .then(trial(1)).then(trial(1)).then(trial(2)).then(trial(1))
      .then(decide(Accept))
      .then(all { assert(verdict == Invalid), trial(1).fail() })
}
