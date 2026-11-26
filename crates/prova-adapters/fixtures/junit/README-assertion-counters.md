# Real JUnit output of assertion-counting reporters

Both files are the tools' own published reporter output; only text that is
not the reporter's XML was taken out, as noted.

- `catch2-selftest.xml`: Catch2's approved JUnit baseline
  `tests/SelfTest/Baselines/junit.sw.approved.txt` from
  https://github.com/catchorg/Catch2 (branch `devel`, fetched 2026-11-26).
  Its placeholders are filled in (`<exe-name>` -> `SelfTest`,
  `<line number>` -> `42`, `<hex digits>` -> `0x1234`, `{duration}` ->
  `0.001`, `{iso8601-timestamp}` -> a fixed time) and the line `loose text
  artifact`, which one of Catch2's self-tests writes to stdout in the middle
  of the XML, is removed. `tests="2445"` counts assertions; the file holds
  1045 test cases.
- `doctest-assertion-macros.xml`: doctest's
  `examples/all_features/test_output/assertion_macros.cpp_junit.txt` from
  https://github.com/doctest/doctest (branch `master`, fetched 2026-11-26),
  without the example program's own last line `Program code.` printed after
  the report. `tests="98"` counts assertions; the file holds 23 test cases.
