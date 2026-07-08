# persona-wire-core::application::doctor::finding

Finding — wire_doctor が emit する 1 件の NG 観察。

design.md §4 / §5 / §6 / §7 に対応する core types。
Kind は内部 closed Enum (新 kind 追加で variant + Probe 同時編集を強制)。

## Types

- `Axis` — (no documentation)
- `Finding` — (no documentation)
- `Kind` — `kind` enum: closed set. 1 variant = 1 Probe が default。
- `Location` — 発生場所 — 固有名詞で指差すための field bundle。 全 Optional。
- `Severity` — (no documentation)

