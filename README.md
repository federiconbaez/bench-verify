# bench-verify

> **ES:** CLI en Rust que vuelve a correr un claim de benchmark N veces, calcula la mediana y un intervalo de confianza bootstrap, y emite PASS/FAIL contra un umbral en TOML. Salida JSON.
> **EN:** Rust CLI that re-runs a benchmark claim N times, computes the median and a bootstrap CI, and emits PASS/FAIL against a TOML threshold. JSON output.
>
> **Estado / Status:** tests unitarios + CLI en verde en la box; **no auditado** por un tercero / **not independently audited**. Licencia / License: Apache-2.0.

## Why

README throughput numbers drift. `bench-verify` turns a claim into a
reproducible check: same command, N samples, median + percentile bootstrap
CI, then a hard PASS/FAIL against a threshold you wrote down.

## Install

```bash
cargo install --path .
```

## Claim file

```toml
name = "demo-pass"
command = "echo 'thrpt: 42.0 Melem/s'"
metric_regex = 'thrpt:\s*([0-9.]+)'
runs = 5

[threshold]
min = 40.0
```

Threshold forms: `min`, `max`, or `range = { lo = …, hi = … }`.

## Usage

```bash
# Against an existing checkout
bench-verify examples/demo-pass.toml -C .

# Clean clone (temp dir), then N runs inside it
bench-verify claim.toml --git https://github.com/federiconbaez/lockfree-mpmc

# JSON only (exit 0 = PASS, 1 = FAIL, 2 = tool error)
bench-verify claim.toml --quiet
```

## Worked example: lockfree-mpmc 1p1c

Published figure ([lockfree-mpmc/BENCH.md](https://github.com/federiconbaez/lockfree-mpmc/blob/master/BENCH.md),
box, Intel Xeon 8 vCPU, Linux 6.12, 2026-10-22 America/Buenos_Aires):

| Config | lockfree-mpmc | crossbeam | std mpsc |
|--------|--------------:|----------:|---------:|
| 1p1c   | **30.8 M/s**  | 5.38 M/s  | 52.5 M/s |

Claim checked in by this repo (`examples/lockfree-mpmc-1p1c.toml`):

- command: `cargo bench --bench throughput` filtered to `lockfree_mpmc/1p1c`
- metric: middle criterion `thrpt` value (Melem/s)
- runs: 3
- threshold: **median ≥ 20.0** M/s

20 M/s is deliberately below the published 30.8 so a noisy shared VM can
still PASS; clearing 20 does **not** re-certify 30.8. To tighten the claim,
raise `min` and re-run on dedicated hardware.

```bash
bench-verify examples/lockfree-mpmc-1p1c.toml \
  --git https://github.com/federiconbaez/lockfree-mpmc
```

Example JSON shape:

```json
{
  "name": "lockfree-mpmc-1p1c",
  "verdict": "PASS",
  "samples": [29.1, 31.0, 30.4],
  "median": 30.4,
  "ci_low": 29.1,
  "ci_high": 31.0,
  "confidence": 0.95,
  "threshold": "median >= 20",
  "reason": "median 30.4 satisfies median >= 20 (CI [29.1, 31.0])"
}
```

(Numbers above are illustrative of the schema; run the tool for real ones.)

## Checks

```bash
cargo test
cargo clippy --all-targets -- -D warnings
cargo run -- examples/demo-pass.toml
```

## License

Apache-2.0
