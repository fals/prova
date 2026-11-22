// Experiment comparison lifecycle (ADR 0007, 0011).
//
// One comparison measures a baseline and a candidate under a protocol that
// freezes at the first trial. A verdict needs the minimum trials on both
// arms, and is only valid when all trials ran on the same machine
// fingerprint. Trials are append-only.
//
// Specification gate (ADR 0015): before any trial, each arm is checked
// against the problem's specification (an oracle command). Trials need
// both arms to satisfy it; a candidate that violates it is rejected
// without measurement, and can never be accepted. A baseline that violates
// it ends the comparison without a verdict (no action is enabled).
module experiment {
  type Arm = Baseline | Candidate
  type Verdict = NoVerdict | Accept | Reject | Inconclusive | Invalid
  type Check = Unchecked | Holds | Violated

  pure val MIN_TRIALS = 2
  pure val MAX_TRIALS = 4
  pure val FINGERPRINTS = Set(1, 2)

  var protocol: int
  var frozenProtocol: int
  var baseTrials: int
  var candTrials: int
  var fingerprints: Set[int]
  var verdict: Verdict
  var baseSpec: Check
  var candSpec: Check
  // Content ids: the snapshot recorded for the candidate, the live working
  // tree, and the content the candidate arm actually ran (0 = not yet run).
  var recorded: int
  var workTree: int
  var measured: int

  action init = all {
    protocol' = 0,
    frozenProtocol' = 0,
    baseTrials' = 0,
    candTrials' = 0,
    fingerprints' = Set(),
    verdict' = NoVerdict,
    baseSpec' = Unchecked,
    candSpec' = Unchecked,
    recorded' = 1,
    workTree' = 1,
    measured' = 0,
  }

  pure def total(b: int, c: int): int = b + c

  // The protocol may change only before the first trial.
  action setProtocol(v: int): bool = all {
    total(baseTrials, candTrials) == 0,
    protocol' = v,
    frozenProtocol' = v,
    baseTrials' = baseTrials, candTrials' = candTrials,
    fingerprints' = fingerprints, verdict' = verdict,
    baseSpec' = baseSpec, candSpec' = candSpec,
    recorded' = recorded, workTree' = workTree, measured' = measured,
  }

  // Each arm is checked once, before any trial.
  action checkSpec(arm: Arm, holds: bool): bool = all {
    total(baseTrials, candTrials) == 0,
    verdict == NoVerdict,
    val result = if (holds) Holds else Violated
    if (arm == Baseline)
      all { baseSpec == Unchecked, baseSpec' = result, candSpec' = candSpec }
    else
      all { candSpec == Unchecked, candSpec' = result, baseSpec' = baseSpec },
    protocol' = protocol, frozenProtocol' = frozenProtocol,
    baseTrials' = baseTrials, candTrials' = candTrials,
    fingerprints' = fingerprints, verdict' = verdict,
    recorded' = recorded, workTree' = workTree,
    // The candidate's oracle runs in its snapshot checkout, not the live tree.
    measured' = if (arm == Candidate) recorded else measured,
  }

  // A candidate that violates the specification is rejected unmeasured.
  action rejectViolation: bool = all {
    verdict == NoVerdict,
    baseSpec == Holds,
    candSpec == Violated,
    verdict' = Reject,
    protocol' = protocol, frozenProtocol' = frozenProtocol,
    baseTrials' = baseTrials, candTrials' = candTrials,
    fingerprints' = fingerprints, baseSpec' = baseSpec, candSpec' = candSpec,
    recorded' = recorded, workTree' = workTree, measured' = measured,
  }

  // Trials alternate arms (interleaving); the next arm is the one behind.
  action trial(fp: int): bool = all {
    verdict == NoVerdict or verdict == Inconclusive,
    baseSpec == Holds,
    candSpec == Holds,
    total(baseTrials, candTrials) < 2 * MAX_TRIALS,
    fingerprints' = fingerprints.union(Set(fp)),
    if (baseTrials <= candTrials)
      all { baseTrials' = baseTrials + 1, candTrials' = candTrials }
    else
      all { baseTrials' = baseTrials, candTrials' = candTrials + 1 },
    verdict' = NoVerdict,
    protocol' = protocol, frozenProtocol' = frozenProtocol,
    baseSpec' = baseSpec, candSpec' = candSpec,
    recorded' = recorded, workTree' = workTree, measured' = recorded,
  }

  // The live working tree may change at any time, including mid-comparison.
  action editWorkTree(v: int): bool = all {
    workTree' = v,
    recorded' = recorded, measured' = measured,
    protocol' = protocol, frozenProtocol' = frozenProtocol,
    baseTrials' = baseTrials, candTrials' = candTrials,
    fingerprints' = fingerprints, verdict' = verdict,
    baseSpec' = baseSpec, candSpec' = candSpec,
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
    baseSpec' = baseSpec, candSpec' = candSpec,
    recorded' = recorded, workTree' = workTree, measured' = measured,
  }

  action step = {
    nondet v = 1.to(3).oneOf()
    nondet fp = FINGERPRINTS.oneOf()
    nondet o = Set(Accept, Reject, Inconclusive).oneOf()
    nondet w = 1.to(3).oneOf()
    nondet arm = Set(Baseline, Candidate).oneOf()
    // Holds more often than not, so long traces still reach decisions.
    nondet holds = Set(true, true, false).oneOf()
    any { setProtocol(v), checkSpec(arm, holds), rejectViolation, trial(fp), decide(o), editWorkTree(w) }
  }

  // ---------------- invariants

  val verdictNeedsEvidence =
    (verdict != NoVerdict) implies
      ((baseTrials >= MIN_TRIALS and candTrials >= MIN_TRIALS) or candSpec == Violated)

  val trialsNeedTheSpec =
    total(baseTrials, candTrials) > 0 implies (baseSpec == Holds and candSpec == Holds)

  val violatorNeverAccepted =
    candSpec == Violated implies (verdict == NoVerdict or verdict == Reject)

  val violatingBaselineDecidesNothing =
    baseSpec == Violated implies verdict == NoVerdict

  val protocolFrozen =
    total(baseTrials, candTrials) > 0 implies protocol == frozenProtocol

  val armsInterleaved =
    baseTrials - candTrials <= 1 and candTrials <= baseTrials

  val mixedMachinesAreInvalid =
    (verdict != NoVerdict and fingerprints.size() > 1) implies verdict == Invalid

  // What ran is what was recorded, whatever the working tree became.
  val measuredIsRecorded = measured == 0 or measured == recorded

  val boundedTrials = baseTrials <= MAX_TRIALS and candTrials <= MAX_TRIALS

  val safety = all {
    verdictNeedsEvidence,
    protocolFrozen,
    armsInterleaved,
    mixedMachinesAreInvalid,
    boundedTrials,
    trialsNeedTheSpec,
    violatorNeverAccepted,
    violatingBaselineDecidesNothing,
    measuredIsRecorded,
  }



  // Regression (ADR 0011 example 2): peeking before the minimum is refused.
  run peekingIsRefusedTest =
    init.then(setProtocol(2)).then(checkSpec(Baseline, true)).then(checkSpec(Candidate, true))
      .then(trial(1)).then(trial(1)).then(trial(1)).then(decide(Accept)).fail()

  // Regression: the protocol cannot move after the first trial.
  run goalpostsCannotMoveTest =
    init.then(setProtocol(2)).then(checkSpec(Baseline, true)).then(checkSpec(Candidate, true))
      .then(trial(1)).then(setProtocol(3)).fail()

  // Trials from two machines make any verdict invalid.
  run mixedMachinesTest =
    init.then(checkSpec(Baseline, true)).then(checkSpec(Candidate, true))
      .then(trial(1)).then(trial(1)).then(trial(2)).then(trial(1))
      .then(decide(Accept))
      .then(all { assert(verdict == Invalid), trial(1).fail() })

  // No trial before both arms satisfy the specification.
  run noMeasurementBeforeTheSpecTest =
    init.then(checkSpec(Baseline, true)).then(trial(1)).fail()

  // A violating candidate is rejected unmeasured and cannot be measured after.
  run violatorIsRejectedTest =
    init.then(checkSpec(Baseline, true)).then(checkSpec(Candidate, false))
      .then(rejectViolation)
      .then(all { assert(verdict == Reject), trial(1).fail() })

  // A violating baseline: nothing can be decided.
  run violatingBaselineTest =
    init.then(checkSpec(Baseline, false)).then(checkSpec(Candidate, true))
      .then(any { rejectViolation, trial(1), decide(Accept) }).fail()

  // Regression: editing the working tree after recording never changes what
  // is measured (the bug: the best arm ran whatever the tree held now).
  run editsDoNotChangeWhatIsMeasuredTest =
    init.then(checkSpec(Baseline, true)).then(editWorkTree(3)).then(checkSpec(Candidate, true))
      .then(trial(1)).then(editWorkTree(2)).then(trial(1)).then(trial(1)).then(trial(1))
      .then(all { assert(measured == recorded and workTree != recorded), decide(Accept) })
}
