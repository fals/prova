# Vision

The original problem statement for prova, as written by its author. The [README](../README.md) summarises what prova does today.

## Test

Over the past years I grew tired of how LLMs execute tests. Through the harness, LLMs tend to execute tests with random bash commands and dump the outcome in a file to analyse afterwards. This implies two main things. First, there is no structure in what is executed: the parameters and variables get lost when iterating over and over, so there is no memory of what was done, of what I executed and its outcome, of the failures of previous iterations, and so on. Second, the way tests are executed is not reliable and is always constrained by long timeouts instead of failing safe, which leads to long loops. The harness MUST be able to see problems fast, abort, fix and reiterate instead of being bound by time, and it must record all this so it can recover and analyse the iterations.

So the best way is a framework, plus skills for each programming language, that guides the LLM to drive itself to a single point of execution that outputs test results in a known format, probably the most used one, JUnit, and a local memory that tracks failures over time.

This should be formalized: a test is a function that the harness executes on the system under test, with parameters, and that outputs something that can be watched, but it is never 100% dependent on bounded time constraints.

Another problem is test sizes. LLM models tend to add very broad and big tests that are slow and flaky; Google's engineering practices already show that small, medium and large tests are the way to go.

## Experiments

This is another problem. When creating new features or trying to innovate, load testing, or improving performance, which needs benchmarking and a memory of before and after for a certain part of the system, the LLM makes ad hoc changes, runs only tests, and does not record progress, outcomes, weights or the inputs used. This leads to a non-deterministic way of iterating over the problem the system under test is meant to fix.

For this the solution is to use the pure scientific method: formalize what we need to achieve, research the problem deeply, and do what LLMs are good at: brute force, repeat. It is cheap, but with formalization it is better.

## Formal Methods

Lean might be a good candidate to formalize what we need in the engine; this needs research. Then we guide the LLM through a well-defined skill for each category: tests, experiments or improvement loops, with a contract that aligns with any test framework out there.

## The system we want to build

Prova will probably be written in Rust, as it is the best fit. It should be easy to deploy, so always a container. It should have proper storage that supports the data contracts we need; an analysis engine that keeps a track record of the statistics the model will need over time; a set of skills for the most known programming languages that teach how to adopt and use prova in the current system under test; and last, a fast, event-driven conduit that the harness uses to communicate with this system. Today MCP is the best option, but we must keep in mind the constraints I referred to, if it is the best option, for fast feedback.
