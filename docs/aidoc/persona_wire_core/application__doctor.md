# persona-wire-core::application::doctor

wire_doctor module — Finding-driven 2-axis (graph / workflow) health
diagnostic. Each Probe emits Severity-tagged Finding(s); [`run`] aggregates
a verdict (HEALTHY / DEGRADED / BROKEN) and renders a Markdown report.
Two modes: Full (`persona_id = None`) / Persona-scoped (`Some(id)`).

Entry: [`run`] is a thin transport. Internally it drives the probe
registry → FindingSink → render pipeline.

## Functions

- `run` — design §3: Full mode (`persona_id = None`) / Persona-scoped mode (`Some(id)`)。

