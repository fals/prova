//! Benchmark metrics from native benchmark tools, for `toe exp`.
//!
//! A trial's whole output is scanned; every recognized benchmark result
//! becomes a metric. Times are normalized to nanoseconds, because tools
//! change their display unit with speed (criterion prints `ms` for the
//! baseline and `µs` for a faster candidate). When a tool prints the same
//! metric several times in one trial (`go test -count 5`), the median is
//! kept.
//!
//! | Tool | Example metric |
//! |---|---|
//! | JSON lines `{"metric": .., "value": .., "unit": ..}` | as printed |
//! | Go `go test -bench` | `BenchmarkCountPrimes.ns_per_op`, `.bytes_per_op`, `.allocs_per_op` |
//! | Rust criterion | `count_primes.time_ns` |
//! | pytest-benchmark | `test_count_primes.mean_ns`, `.median_ns` |
//! | Vitest bench | `countPrimes.mean_ns`, `countPrimes.hz` |
//! | JMH | `PrimesBench.countPrimes.ns_per_op` (time modes) or `.ops_per_s` (throughput) |
//! | BenchmarkDotNet | `CountPrimes.mean_ns`, `CountPrimes.median_ns` |

use std::collections::BTreeMap;

/// One measured value: (metric name, value, unit).
pub type Metric = (String, f64, Option<String>);

/// Nanoseconds per unit of a time unit as tools print it.
fn ns_per(unit: &str) -> Option<f64> {
    Some(match unit.trim() {
        "ns" => 1.0,
        "us" | "µs" | "μs" => 1e3,
        "ms" => 1e6,
        "s" | "sec" => 1e9,
        "min" => 60e9,
        _ => return None,
    })
}

fn num(s: &str) -> Option<f64> {
    s.trim()
        .replace(',', "")
        .parse::<f64>()
        .ok()
        .filter(|v| v.is_finite())
}

/// Every metric found in a benchmark run's output, one value per name.
pub fn parse(output: &str) -> Vec<Metric> {
    let mut raw: Vec<Metric> = Vec::new();
    let lines: Vec<&str> = output.lines().collect();
    json_lines(&lines, &mut raw);
    go(&lines, &mut raw);
    criterion(&lines, &mut raw);
    pytest_benchmark(&lines, &mut raw);
    vitest(&lines, &mut raw);
    jmh(&lines, &mut raw);
    benchmarkdotnet(&lines, &mut raw);
    median_per_metric(raw)
}

fn median_per_metric(raw: Vec<Metric>) -> Vec<Metric> {
    let mut by: BTreeMap<String, (Vec<f64>, Option<String>)> = BTreeMap::new();
    for (name, v, unit) in raw {
        let e = by.entry(name).or_insert_with(|| (Vec::new(), unit.clone()));
        e.0.push(v);
    }
    by.into_iter()
        .map(|(name, (mut vs, unit))| {
            vs.sort_by(f64::total_cmp);
            let n = vs.len();
            let m = if n % 2 == 1 {
                vs[n / 2]
            } else {
                (vs[n / 2 - 1] + vs[n / 2]) / 2.0
            };
            (name, m, unit)
        })
        .collect()
}

fn json_lines(lines: &[&str], out: &mut Vec<Metric>) {
    for l in lines {
        let t = l.trim();
        if !t.starts_with('{') {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(t) else {
            continue;
        };
        if let (Some(name), Some(value)) = (v["metric"].as_str(), v["value"].as_f64())
            && value.is_finite()
        {
            out.push((
                name.to_owned(),
                value,
                v["unit"].as_str().map(str::to_owned),
            ));
        }
    }
}

/// `BenchmarkX-32   64   18538883 ns/op   0 B/op   0 allocs/op` (plus any
/// `b.ReportMetric` pairs such as `12.5 MB/s` or `3.0 hits/op`).
fn go(lines: &[&str], out: &mut Vec<Metric>) {
    for l in lines {
        let mut f = l.split_whitespace();
        let Some(name) = f.next().filter(|n| n.starts_with("Benchmark")) else {
            continue;
        };
        let Some(_iterations) = f.next().and_then(|n| n.parse::<u64>().ok()) else {
            continue;
        };
        let name = match name.rsplit_once('-') {
            Some((base, procs)) if procs.chars().all(|c| c.is_ascii_digit()) => base,
            _ => name,
        };
        let rest: Vec<&str> = f.collect();
        for pair in rest.chunks(2) {
            let [value, unit] = pair else { break };
            let Some(v) = num(value) else { break };
            let (metric, unit_out) = match *unit {
                "ns/op" => ("ns_per_op".to_owned(), "ns"),
                "B/op" => ("bytes_per_op".to_owned(), "B"),
                "allocs/op" => ("allocs_per_op".to_owned(), "allocs"),
                "MB/s" => ("mb_per_s".to_owned(), "MB/s"),
                other => (
                    other
                        .replace('/', "_per_")
                        .replace(|c: char| !c.is_ascii_alphanumeric() && c != '_', "_"),
                    *unit,
                ),
            };
            out.push((format!("{name}.{metric}"), v, Some(unit_out.to_owned())));
        }
    }
}

/// `count_primes            time:   [16.160 ms 16.441 ms 16.813 ms]`: the
/// middle value is criterion's point estimate.
fn criterion(lines: &[&str], out: &mut Vec<Metric>) {
    for l in lines {
        let Some((name, rest)) = l.split_once("time:") else {
            continue;
        };
        let name = name.trim();
        let Some(inner) = rest
            .trim()
            .strip_prefix('[')
            .and_then(|r| r.strip_suffix(']'))
        else {
            continue;
        };
        let parts: Vec<&str> = inner.split_whitespace().collect();
        if name.is_empty() || parts.len() != 6 {
            continue;
        }
        if let (Some(v), Some(scale)) = (num(parts[2]), ns_per(parts[3])) {
            out.push((format!("{name}.time_ns"), v * scale, Some("ns".into())));
        }
    }
}

/// pytest-benchmark table: header `Name (time in ms)  Min Max Mean Median ...`.
fn pytest_benchmark(lines: &[&str], out: &mut Vec<Metric>) {
    let mut cols: Option<(Vec<String>, f64)> = None;
    for l in lines {
        let t = l.trim();
        if let Some(rest) = t.strip_prefix("Name (time in ") {
            let Some((unit, header)) = rest.split_once(')') else {
                continue;
            };
            let Some(scale) = ns_per(unit) else { continue };
            cols = Some((
                header
                    .split_whitespace()
                    .map(|c| c.to_ascii_lowercase())
                    .collect(),
                scale,
            ));
            continue;
        }
        if t.starts_with("---") || t.is_empty() || t.starts_with("Legend") {
            if t.starts_with("Legend") {
                cols = None;
            }
            continue;
        }
        let Some((names, scale)) = &cols else {
            continue;
        };
        let mut f = t.split_whitespace();
        let Some(test) = f.next() else { continue };
        // Values may carry outlier marks like "1.2 (1.10)"; keep plain numbers.
        let values: Vec<f64> = f.filter_map(num).collect();
        for (col, v) in names.iter().zip(values) {
            if col == "mean" || col == "median" {
                out.push((format!("{test}.{col}_ns"), v * scale, Some("ns".into())));
            }
        }
    }
}

/// Vitest bench: header `name hz min max mean p75 ...`, rows `· countPrimes ...`
/// (times in ms).
fn vitest(lines: &[&str], out: &mut Vec<Metric>) {
    let mut header: Option<Vec<String>> = None;
    for l in lines {
        let t = l.trim();
        let words: Vec<&str> = t.split_whitespace().collect();
        if words.first() == Some(&"name") && words.contains(&"hz") && words.contains(&"mean") {
            header = Some(words[1..].iter().map(|w| w.to_string()).collect());
            continue;
        }
        let Some(cols) = &header else { continue };
        let Some(row) = t
            .strip_prefix('·')
            .or_else(|| t.strip_prefix("✓"))
            .map(str::trim)
        else {
            continue;
        };
        let mut f = row.split_whitespace();
        let Some(name) = f.next() else { continue };
        let values: Vec<Option<f64>> = f.map(num).collect();
        for (col, v) in cols.iter().zip(values) {
            let Some(v) = v else { continue };
            match col.as_str() {
                "mean" => out.push((format!("{name}.mean_ns"), v * 1e6, Some("ns".into()))),
                "hz" => out.push((format!("{name}.hz"), v, Some("ops/s".into()))),
                _ => {}
            }
        }
    }
}

/// JMH result table: `Benchmark  Mode  Cnt  Score   Error  Units`, rows like
/// `calc.PrimesBench.countPrimes  avgt  3  16.123 ± 0.456  ms/op`.
fn jmh(lines: &[&str], out: &mut Vec<Metric>) {
    let mut in_table = false;
    for l in lines {
        let w: Vec<&str> = l.split_whitespace().collect();
        if w.first() == Some(&"Benchmark") && w.contains(&"Mode") && w.contains(&"Score") {
            in_table = true;
            continue;
        }
        // name, mode, [cnt], score, [±, error], unit
        if !in_table || w.len() < 4 {
            continue;
        }
        let (name, unit) = (w[0], w[w.len() - 1]);
        let score_at = if w[2].parse::<u64>().is_ok() && w.len() > 4 && num(w[3]).is_some() {
            3
        } else {
            2
        };
        let Some(score) = num(w[score_at]) else {
            continue;
        };
        let Some((left, right)) = unit.split_once('/') else {
            continue;
        };
        if right == "op"
            && let Some(scale) = ns_per(left)
        {
            out.push((
                format!("{name}.ns_per_op"),
                score * scale,
                Some("ns".into()),
            ));
        } else if left == "ops"
            && let Some(scale) = ns_per(right)
        {
            out.push((
                format!("{name}.ops_per_s"),
                score * 1e9 / scale,
                Some("ops/s".into()),
            ));
        }
    }
}

/// BenchmarkDotNet summary table (Markdown-like):
/// `| Method      | Mean     | Error    | StdDev   |` then
/// `| CountPrimes | 16.12 ms | 0.123 ms | 0.045 ms |`.
fn benchmarkdotnet(lines: &[&str], out: &mut Vec<Metric>) {
    let mut header: Option<Vec<String>> = None;
    for l in lines {
        let t = l.trim();
        if !t.starts_with('|') {
            header = None;
            continue;
        }
        let cells: Vec<String> = t
            .trim_matches('|')
            .split('|')
            .map(|c| c.trim().to_owned())
            .collect();
        if cells.first().map(String::as_str) == Some("Method") {
            header = Some(cells);
            continue;
        }
        let Some(h) = &header else { continue };
        if cells
            .iter()
            .all(|c| c.chars().all(|ch| ch == '-' || ch == ':'))
        {
            continue;
        }
        let name = &cells[0];
        for (col, cell) in h.iter().zip(&cells) {
            let key = match col.as_str() {
                "Mean" => "mean_ns",
                "Median" => "median_ns",
                _ => continue,
            };
            let mut parts = cell.split_whitespace();
            if let (Some(v), Some(unit)) = (parts.next().and_then(num), parts.next())
                && let Some(scale) = ns_per(unit)
            {
                out.push((format!("{name}.{key}"), v * scale, Some("ns".into())));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/fixtures/bench/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    fn get(m: &[Metric], name: &str) -> f64 {
        m.iter()
            .find(|x| x.0 == name)
            .unwrap_or_else(|| panic!("{name} not in {m:?}"))
            .1
    }

    #[test]
    fn go_bench_takes_the_median_of_counts_and_strips_procs() {
        let m = parse(&fixture("go-bench.txt"));
        assert_eq!(get(&m, "BenchmarkCountPrimes.ns_per_op"), 18538883.0);
        assert_eq!(get(&m, "BenchmarkCountPrimes.allocs_per_op"), 0.0);
        assert!(m.iter().all(|x| !x.0.contains("-32")));
    }

    #[test]
    fn criterion_point_estimate_in_ns() {
        let m = parse(&fixture("criterion.txt"));
        assert!((get(&m, "count_primes.time_ns") - 30_598_000.0).abs() < 1.0);
        // A faster candidate prints microseconds: same metric, same unit.
        let fast = parse("count_primes            time:   [630.55 µs 633.21 µs 636.09 µs]");
        assert!((get(&fast, "count_primes.time_ns") - 633_210.0).abs() < 1.0);
    }

    #[test]
    fn pytest_benchmark_mean_and_median() {
        let m = parse(&fixture("pytest-benchmark.txt"));
        assert!((get(&m, "test_count_primes.mean_ns") - 20_141_500.0).abs() < 1.0);
        assert!((get(&m, "test_count_primes.median_ns") - 20_100_300.0).abs() < 1.0);
    }

    #[test]
    fn vitest_bench_mean_and_hz() {
        let m = parse(&fixture("vitest-bench.txt"));
        assert!((get(&m, "countPrimes.mean_ns") - 16_410_100.0).abs() < 1.0);
        assert!((get(&m, "countPrimes.hz") - 60.9382).abs() < 1e-9);
    }

    #[test]
    fn jmh_average_time_and_throughput() {
        let m = parse(&fixture("jmh.txt"));
        assert!(
            (get(&m, "PrimesBench.countPrimes.ns_per_op") - 15_632_000.0).abs() < 1.0,
            "{m:?}"
        );
        let thr = parse(
            "Benchmark          Mode  Cnt    Score   Error   Units\nB.x  thrpt    3  120.5 ± 3.1  ops/ms",
        );
        assert!((get(&thr, "B.x.ops_per_s") - 120_500.0).abs() < 1e-6);
    }

    #[test]
    fn benchmarkdotnet_mean() {
        let m = parse(&fixture("benchmarkdotnet.txt"));
        let mean = get(&m, "CountPrimes.mean_ns");
        assert!((15_000_000.0..16_500_000.0).contains(&mean), "{mean}");
    }

    #[test]
    fn json_lines_and_unrelated_output() {
        let m = parse(
            "{\"metric\":\"p99_ms\",\"value\":12.5,\"unit\":\"ms\"}\nBenchmarking is fun\n| not | a table |",
        );
        assert_eq!(
            m,
            vec![("p99_ms".to_string(), 12.5, Some("ms".to_string()))]
        );
        assert!(parse("ok  \texample.com/calc\t1.5s\ntest result: ok. 3 passed").is_empty());
    }
}
