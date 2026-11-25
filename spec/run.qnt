// Run lifecycle (ADR 0002, 0004, 0011).
//
// A run is created with immutable inputs, started, observes test events,
// and ends exactly once: finished, cancelled, or aborted. Events arriving
// after the end are rejected and counted, never recorded.
//
// Verdict (ADR 0017): a pass must be earned. The run's verdict is decided at
// the end from all the evidence: the exit code, every test outcome (an open
// test is closed as errored), and contradictions the framework itself
// reports (its summary counts more failures than were seen, it says no test
// ran, or the suite's tests disappeared).
module runLifecycle {
  type Status = Created | Running | Finished | Cancelled | Aborted
  type Outcome = Passed | Failed | Skipped | Errored
  type Verdict = Undecided | Pass | Fail | Error
  type Ev =
    | TestStarted(str)
    | TestFinished({ test: str, outcome: Outcome })
    | RunEnded(Status)
    | Superseded(str)

  // Bounded model: two tests are enough to expose interleavings.
  pure val TESTS = Set("a", "b")

  var status: Status
  var log: List[Ev]
  var open: Set[str]
  var outcomes: str -> Outcome
  var inputs: int
  var frozenInputs: int
  var dropped: int
  var exitCode: int
  var contradiction: bool
  var verdict: Verdict

  // Exit codes the process can end with, in the bounded model: success,
  // failure, could not start (command not found).
  pure val EXITS = Set(0, 1, 127)

  pure def decide(s: Status, code: int, out: str -> Outcome, contra: bool): Verdict =
    if (s != Finished) Error
    else if (code == 126 or code == 127) Error
    else if (out.keys().exists(t => out.get(t) == Failed or out.get(t) == Errored)) Fail
    else if (contra) Fail
    else if (code != 0) Fail
    else Pass

  // Quarantine (ADR 0027): tests set aside from the verdict in the open,
  // fixed when the run is created. On top of `decide`: a run whose only
  // failures are quarantined tests that plainly failed (not errored, not
  // cut off), that finished with no contradiction and a test-failure exit
  // code, passes without them. Anything else keeps the verdict.
  // ADR 0031: only when the command is one test-runner call (`runnerOnly`,
  // fixed with the command when the run is created): a step before, after
  // or between runner calls, or one the failure skipped, is out of sight.
  // ADR 0034: and only when every test the suite's last full pass
  // reported (`known`, fixed when the run is created) has a result: a
  // runner that stopped at the quarantined failure never ran the rest.
  pure val QUARANTINED = Set("b")
  pure def withQuarantine(v: Verdict, s: Status, code: int, out: str -> Outcome,
                          contra: bool, runnerOnly: bool, known: Set[str], q: Set[str]): Verdict =
    if (v == Fail and s == Finished and not(contra) and runnerOnly
        and known.forall(t => out.keys().contains(t))
        and code != 0 and code != 126 and code != 127
        and out.keys().exists(t => out.get(t) == Failed or out.get(t) == Errored)
        and out.keys().forall(t =>
          (out.get(t) != Failed and out.get(t) != Errored)
          or (q.contains(t) and out.get(t) == Failed))
        // Something else was verified: a run of only quarantined tests
        // that failed passes nothing (round 14).
        and out.keys().exists(t => out.get(t) == Passed and not(q.contains(t))))
      Pass
    else v

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
    exitCode' = 0,
    contradiction' = false,
    verdict' = Undecided,
  }

  // Inputs may change only while the run is still being created.
  action setInputs(v: int): bool = all {
    status == Created,
    inputs' = v,
    frozenInputs' = frozenInputs,
    status' = status, log' = log, open' = open, outcomes' = outcomes, dropped' = dropped,
    exitCode' = exitCode, contradiction' = contradiction, verdict' = verdict,
  }

  action start = all {
    status == Created,
    status' = Running,
    frozenInputs' = inputs,
    log' = log, open' = open, outcomes' = outcomes,
    inputs' = inputs, dropped' = dropped,
    exitCode' = exitCode, contradiction' = contradiction, verdict' = verdict,
  }

  pure def isFailure(o: Outcome): bool = o == Failed or o == Errored

  // A test starts once, or again after it failed: a retry (ADR 0024). The
  // retry's outcome replaces the failed one; whether a pass on retry is
  // trusted is decided outside the lifecycle (it is flagged, not green).
  action testStarted(t: str): bool = all {
    status == Running,
    not(open.contains(t)),
    not(outcomes.keys().contains(t)) or isFailure(outcomes.get(t)),
    // A superseded name is gone: its test is the one that replaced it.
    not(log.indices().exists(i => log[i] == Superseded(t))),
    log' = log.append(TestStarted(t)),
    open' = open.union(Set(t)),
    status' = status, outcomes' = outcomes,
    inputs' = inputs, frozenInputs' = frozenInputs, dropped' = dropped,
    exitCode' = exitCode, contradiction' = contradiction, verdict' = verdict,
  }

  action testFinished(t: str, o: Outcome): bool = all {
    status == Running,
    open.contains(t),
    log' = log.append(TestFinished({ test: t, outcome: o })),
    open' = open.exclude(Set(t)),
    outcomes' = outcomes.put(t, o),
    status' = status,
    inputs' = inputs, frozenInputs' = frozenInputs, dropped' = dropped,
    exitCode' = exitCode, contradiction' = contradiction, verdict' = verdict,
  }

  // A test that already has a result is executed again and fails (Go's
  // `-count=N` repeats executions, it does not retry them). The failure is
  // recorded: it must never hide behind an earlier pass. A repeated pass
  // changes nothing and is not an event.
  action testRepeated(t: str, o: Outcome): bool = all {
    status == Running,
    not(open.contains(t)),
    outcomes.keys().contains(t),
    isFailure(o),
    log' = log.append(TestFinished({ test: t, outcome: o })),
    outcomes' = outcomes.put(t, o),
    open' = open, status' = status,
    inputs' = inputs, frozenInputs' = frozenInputs, dropped' = dropped,
    exitCode' = exitCode, contradiction' = contradiction, verdict' = verdict,
  }

  // A result recorded under a provisional name is re-keyed: its test is
  // `by`, whose own result is already recorded (a Go test once its package
  // is known, a console name once the report names it). The provisional
  // entry leaves the outcomes: one test, one result (round 15: its failure
  // stayed in the verdict after a rerun passed).
  action superseded(t: str, by: str): bool = all {
    status == Running,
    t != by,
    not(open.contains(t)),
    outcomes.keys().contains(t),
    outcomes.keys().contains(by),
    outcomes' = outcomes.keys().exclude(Set(t)).mapBy(k => outcomes.get(k)),
    log' = log.append(Superseded(t)), open' = open, status' = status,
    inputs' = inputs, frozenInputs' = frozenInputs, dropped' = dropped,
    exitCode' = exitCode, contradiction' = contradiction, verdict' = verdict,
  }

  // The process exits (before the run is recorded as ended).
  action exited(code: int): bool = all {
    status == Running,
    exitCode' = code,
    status' = status, log' = log, open' = open, outcomes' = outcomes,
    inputs' = inputs, frozenInputs' = frozenInputs, dropped' = dropped,
    contradiction' = contradiction, verdict' = verdict,
  }

  // The framework's own report contradicts a pass.
  action contradicted = all {
    status == Running,
    contradiction' = true,
    status' = status, log' = log, open' = open, outcomes' = outcomes,
    inputs' = inputs, frozenInputs' = frozenInputs, dropped' = dropped,
    exitCode' = exitCode, verdict' = verdict,
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
      exitCode' = exitCode, contradiction' = contradiction,
      verdict' = decide(s, exitCode, closing, contradiction),
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
    exitCode' = exitCode, contradiction' = contradiction, verdict' = Error,
  }

  // A test event that arrives after the end: rejected, counted.
  action late(t: str, o: Outcome): bool = all {
    isTerminal(status),
    dropped' = dropped + 1,
    status' = status, log' = log, open' = open, outcomes' = outcomes,
    inputs' = inputs, frozenInputs' = frozenInputs,
    exitCode' = exitCode, contradiction' = contradiction, verdict' = verdict,
  }

  action step = {
    nondet t = TESTS.oneOf()
    nondet b = TESTS.oneOf()
    nondet o = Set(Passed, Failed, Skipped, Errored).oneOf()
    nondet s = Set(Finished, Cancelled, Aborted).oneOf()
    nondet v = 1.to(3).oneOf()
    nondet code = EXITS.oneOf()
    any {
      exited(code),
      contradicted,
      setInputs(v),
      start,
      testStarted(t),
      testFinished(t, o),
      testRepeated(t, o),
      superseded(t, b),
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

  // A test starts again only after a failed attempt: the last result
  // recorded for it before each start is a failure.
  pure def finishedOf(e: Ev, t: str): bool = match e {
    | TestFinished(f) => f.test == t
    | _ => false
  }
  pure def failedEv(e: Ev): bool = match e {
    | TestFinished(f) => isFailure(f.outcome)
    | _ => false
  }
  val retriesFollowFailures =
    log.indices().forall(i => match log[i] {
      | TestStarted(t) => log.indices().forall(j =>
          (j < i and finishedOf(log[j], t)
            and not(log.indices().exists(k => k > j and k < i and finishedOf(log[k], t))))
          implies failedEv(log[j]))
      | _ => true
    })

  // Inputs never change after creation.
  val inputsFrozen = status != Created implies inputs == frozenInputs

  // A pass is earned: the process exited 0, no test failed or errored (an
  // unfinished test counts as errored), and nothing contradicts it.
  val passIsEarned =
    verdict == Pass implies all {
      status == Finished,
      exitCode == 0,
      outcomes.keys().forall(t => outcomes.get(t) != Failed and outcomes.get(t) != Errored),
      not(contradiction),
    }

  // A quarantine never hides anything but its own tests' plain failures:
  // a pass it grants has a finished run, no contradiction, every other
  // test passed or skipped, and no quarantined test errored.
  val quarantineHidesOnlyItsOwn = Set(true, false).forall(runnerOnly =>
    Set(Set(), Set("a"), Set("a", "b"), Set("a", "c")).forall(known =>
    withQuarantine(verdict, status, exitCode, outcomes, contradiction, runnerOnly, known, QUARANTINED) == Pass
      implies all {
        status == Finished,
        not(contradiction),
        exitCode != 126 and exitCode != 127,
        outcomes.keys().forall(t =>
          (outcomes.get(t) != Failed and outcomes.get(t) != Errored)
          or (QUARANTINED.contains(t) and outcomes.get(t) == Failed)),
        // A pass the quarantine granted verified some other test, and the
        // command was one test-runner call (ADR 0031).
        verdict != Pass implies
          outcomes.keys().exists(t => outcomes.get(t) == Passed and not(QUARANTINED.contains(t))),
        verdict != Pass implies runnerOnly,
        // Every known test reported (ADR 0034).
        verdict != Pass implies known.forall(t => outcomes.keys().contains(t)),
      }))

  // A command that could not start is not a test failure.
  val couldNotStartIsAnError =
    (status == Finished and (exitCode == 126 or exitCode == 127)) implies verdict == Error

  // The verdict exists exactly when the run ended.
  val verdictAtEnd = isTerminal(status) iff verdict != Undecided

  val safety = all {
    passIsEarned,
    quarantineHidesOnlyItsOwn,
    couldNotStartIsAnError,
    verdictAtEnd,
    nothingAfterEnd,
    endMatchesStatus,
    terminalHasNoOpenTests,
    outcomesOnlyForStarted,
    inputsFrozen,
    retriesFollowFailures,
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

  // ADR 0017 case 8: a test that never finished fails the run, exit 0 or not.
  run unfinishedTestFailsTest =
    init.then(start).then(testStarted("a")).then(exited(0)).then(end(Finished))
      .then(all { assert(verdict == Fail), assert(safety), late("a", Passed) })

  // ADR 0017 case 9: the framework's summary beats its exit code.
  run contradictionFailsTest =
    init.then(start).then(testStarted("a")).then(testFinished("a", Passed))
      .then(contradicted).then(exited(0)).then(end(Finished))
      .then(all { assert(verdict == Fail), late("a", Passed) })

  // ADR 0017 case 15: a command that could not start is an error.
  run notFoundIsAnErrorTest =
    init.then(start).then(exited(127)).then(end(Finished))
      .then(all { assert(verdict == Error), late("a", Passed) })

  // ADR 0024: a test that failed and then passed on a retry passes; the
  // run's verdict follows each test's last attempt.
  run retryPassTest =
    init.then(start).then(testStarted("a")).then(testFinished("a", Failed))
      .then(testStarted("a")).then(testFinished("a", Passed))
      .then(exited(0)).then(end(Finished))
      .then(all { assert(verdict == Pass), assert(safety), late("a", Passed) })

  // Go -count=3: pass, fail, pass of one test. The failure is recorded after
  // the pass, and the last execution is what the test ends as.
  run repeatedFailureTest =
    init.then(start).then(testStarted("a")).then(testFinished("a", Passed))
      .then(testRepeated("a", Failed))
      .then(all { assert(outcomes.get("a") == Failed), exited(1) })
      .then(end(Finished))
      .then(all { assert(verdict == Fail), assert(safety), late("a", Passed) })

  // A repeated pass is not an event.
  run repeatedPassIsNotAnEventTest =
    init.then(start).then(testStarted("a")).then(testFinished("a", Passed))
      .then(testRepeated("a", Passed)).fail()

  // A passed test does not start again.
  run noRestartAfterPassTest =
    init.then(start).then(testStarted("a")).then(testFinished("a", Passed))
      .then(testStarted("a")).fail()

  // ADR 0027: only a quarantined test failed: the run passes without it.
  run quarantinedFailurePassesTest =
    init.then(start).then(testStarted("a")).then(testFinished("a", Passed))
      .then(testStarted("b")).then(testFinished("b", Failed))
      .then(exited(1)).then(end(Finished))
      .then(all {
        assert(verdict == Fail),
        assert(withQuarantine(verdict, status, exitCode, outcomes, contradiction, true, Set("a", "b"), QUARANTINED) == Pass),
        // ADR 0034: a known test that did not report (a runner stopped at
        // the quarantined failure) leaves the failure standing.
        assert(withQuarantine(verdict, status, exitCode, outcomes, contradiction, true, Set("a", "b", "c"), QUARANTINED) == Fail),
        // ADR 0031: the same failure under a command that does more than
        // run the tests (make, a script, a chain) is not set aside.
        assert(withQuarantine(verdict, status, exitCode, outcomes, contradiction, false, Set("a", "b"), QUARANTINED) == Fail),
        assert(safety),
        late("a", Passed),
      })

  // ADR 0027: another failure, or a quarantined test cut off, still fails.
  run quarantineHidesNothingElseTest =
    init.then(start).then(testStarted("a")).then(testFinished("a", Failed))
      .then(testStarted("b")).then(testFinished("b", Failed))
      .then(exited(1)).then(end(Finished))
      .then(all {
        assert(withQuarantine(verdict, status, exitCode, outcomes, contradiction, true, Set("a", "b"), QUARANTINED) == Fail),
        late("a", Passed),
      })

  // Round 14: only the quarantined test ran, and failed: no pass.
  run onlyQuarantinedFailingIsNoPassTest =
    init.then(start).then(testStarted("b")).then(testFinished("b", Failed))
      .then(exited(1)).then(end(Finished))
      .then(all {
        assert(withQuarantine(verdict, status, exitCode, outcomes, contradiction, true, Set("a", "b"), QUARANTINED) == Fail),
        late("b", Failed),
      })

  // Round 15: a provisional result re-keyed under its real name leaves the
  // outcomes; a failure of it no longer counts once the test passed.
  run supersededResultLeavesTest =
    init.then(start).then(testStarted("a")).then(testFinished("a", Failed))
      .then(testStarted("b")).then(testFinished("b", Failed))
      .then(testStarted("b")).then(testFinished("b", Passed))
      .then(superseded("a", "b"))
      .then(exited(0)).then(end(Finished))
      .then(all { assert(verdict == Pass), assert(safety), late("a", Passed) })

  run cleanPassTest =
    init.then(start).then(testStarted("a")).then(testFinished("a", Passed))
      .then(exited(0)).then(end(Finished))
      .then(all { assert(verdict == Pass), late("a", Passed) })
}
