# Vision

The original problem statement for toe, as written by its author. The
[README](../README.md) summarises what toe does today.


## Test

I grew tired over the past years how LLMs execute test. LLMs via the harness have a tendence of executing tests with random bash script commands and dump the outcome in a file to analyse after. This implicates two main things. First the lack of scructure on what is executed the params and variables gets lost when iterating over and over, so there's no memory of what is done of what I executed and the outcome of that execution, previous iterations failures and so on. Second the way the test execution is done is not reliable and always constraint to long timeouts instead of fail safe, what leads to long loops, harness MUST be able to see fast problems abort fix and reiterate instead of be bound by time, and record this and be able to recover and analyse iterations.

So the best way is a framework and skills for each programing language that will guide the LLM to drive itself to a single point execution that will output from test a know format, probably the most used junit output, and a local memory that will track failures overtime.

This should be formalized, a test is a function that will execute in the system under test by the harness with parameters and will output something that can be watched but never 100% dependend on bounded time constraints.

One other problem is the test sizes, LLM models tend to add very broad and big tests that are slow and flake, Google Enginnering practices already show that small, medium and large tests are the way to go.

## Experiments

This is another problem, when creating a new features or trying to innovate, load testing, improving perfomance that needs benchmarking and memory of before and after af certain part of the system, it does adhoc changes, run only tests and do not record progress outcomes and weights or inputs used, leading to non deterministic way to iterate over a problem the system under test meant to fix.

For this the solution is pure scientific method to be used, formalization of what is needed to archive, deep research of the problem and do what LLMs are god at, bruteforce, repeat. Is cheap, but with formalization is better.

## Lean

Lean might be a good candidate to formalize what we need in the engine, should research, so we guide LLM via well defined skill for each category test, or experiments or improvment loops, a contract that will be align with any test framework out there.

## The system we want to build

Toe (Test oriented experimentation) will be writen probably rust as is best. This should be easy to deploy so always container. It should have a proper storage that will support the data contracts we need, and analysis engine that will keep track record of statistics the model will need overtime, a set of skill that will work with most known programing laguages to teach how to implement and use toe in the current system under test and last a fast eventfull conduit that harness will use to communicate with this system, today MCP is the best but we must keep in mind the constraints I refered, if is the best option, for fast feedback.
