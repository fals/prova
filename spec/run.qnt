// Run lifecycle (ADR 0002, 0004, 0011).
//
// A run is created with immutable inputs, started, observes test events,
// and ends exactly once: finished, cancelled, or aborted. Events arriving
// after the end are rejected and counted, never recorded.
module runLifecycle {
  type Status = Created | Running | Finished | Cancelled | Aborted
  type Outcome = Passed | Failed | Skipped | Errored
  type Ev =
    | TestStarted(str)
    | TestFinished({ test: str, outcome: Outcome })
    | RunEnded(Status)

  // Bounded model: two tests are enough to expose interleavings.
  pure val TESTS = Set("a", "b")

  var status: Status
  var log: List[Ev]
  var open: Set[str]
  var outcomes: str -> Outcome
  var inputs: int
  var frozenInputs: int
  var dropped: int

  pure def isTerminal(s: Status): bool =
    s == Finished or s == Cancelled or s == Aborted

  action init = all {
    status' = Created,
    log' = List(),
    open' = Set(),
    outcomes' = Map(),
    inputs' = 0,
    frozenInputs' = 0,
    dropped' = 0,
  }

  // Inputs may change only while the run is still being created.
  action setInputs(v: int): bool = all {
    status == Created,
    inputs' = v,
    frozenInputs' = frozenInputs,
    status' = status, log' = log, open' = open, outcomes' = outcomes, dropped' = dropped,
  }

  action start = all {
    status == Created,
    status' = Running,
    frozenInputs' = inputs,
    log' = log, open' = open, outcomes' = outcomes,
    inputs' = inputs, dropped' = dropped,
  }

  action testStarted(t: str): bool = all {
    status == Running,
    not(open.contains(t)),
    not(outcomes.keys().contains(t)),
    log' = log.append(TestStarted(t)),
    open' = open.union(Set(t)),
    status' = status, outcomes' = outcomes,
    inputs' = inputs, frozenInputs' = frozenInputs, dropped' = dropped,
  }

  action testFinished(t: str, o: Outcome): bool = all {
    status == Running,
    open.contains(t),
    log' = log.append(TestFinished({ test: t, outcome: o })),
    open' = open.exclude(Set(t)),
    outcomes' = outcomes.put(t, o),
    status' = status,
    inputs' = inputs, frozenInputs' = frozenInputs, dropped' = dropped,
  }

  // Ending closes every open test as errored, then records the end.
  action end(s: Status): bool = all {
    status == Running,
    isTerminal(s),
    val closing = open.fold(outcomes, (m, t) => m.put(t, Errored))
    all {
      outcomes' = closing,
      open' = Set(),
      log' = log.append(RunEnded(s)),
      status' = s,
      inputs' = inputs, frozenInputs' = frozenInputs, dropped' = dropped,
    },
  }

  // Cancel before start is allowed too: nothing ran.
  action cancelCreated = all {
    status == Created,
    status' = Cancelled,
    frozenInputs' = inputs,
    log' = log.append(RunEnded(Cancelled)),
    open' = open, outcomes' = outcomes,
    inputs' = inputs, dropped' = dropped,
  }

  // A test event that arrives after the end: rejected, counted.
  action late(t: str, o: Outcome): bool = all {
    isTerminal(status),
    dropped' = dropped + 1,
    status' = status, log' = log, open' = open, outcomes' = outcomes,
    inputs' = inputs, frozenInputs' = frozenInputs,
  }

  action step = {
    nondet t = TESTS.oneOf()
    nondet o = Set(Passed, Failed, Skipped, Errored).oneOf()
    nondet s = Set(Finished, Cancelled, Aborted).oneOf()
    nondet v = 1.to(3).oneOf()
    any {
      setInputs(v),
      start,
      testStarted(t),
      testFinished(t, o),
      end(s),
      cancelCreated,
      late(t, o),
    }
  }

  // ---------------- invariants

  // Nothing is recorded after the run ended.
  val nothingAfterEnd =
    log.indices().forall(i => match log[i] {
      | RunEnded(_) => i == log.length() - 1
      | _ => true
    })

  // The end is recorded exactly when the status is terminal.
  val endMatchesStatus =
    isTerminal(status) iff (log.length() > 0 and match log[log.length() - 1] {
      | RunEnded(s) => s == status
      | _ => false
    })

  // A terminal run has no open tests.
  val terminalHasNoOpenTests = isTerminal(status) implies open == Set()

  // Each test has at most one outcome, and only tests that started have one.
  val outcomesOnlyForStarted =
    outcomes.keys().forall(t => log.indices().exists(i => log[i] == TestStarted(t)))

  // Inputs never change after creation.
  val inputsFrozen = status != Created implies inputs == frozenInputs

  val safety = all {
    nothingAfterEnd,
    endMatchesStatus,
    terminalHasNoOpenTests,
    outcomesOnlyForStarted,
    inputsFrozen,
  }

  // Regression: the cancel-versus-late-result race from ADR 0011.
  run cancelRaceTest =
    init
      .then(start)
      .then(testStarted("a"))
      .then(end(Cancelled))
      .then(late("a", Passed))
      .then(all {
        assert(safety),
        assert(dropped == 1),
        assert(outcomes.get("a") == Errored),
        late("b", Failed),
      })
}
