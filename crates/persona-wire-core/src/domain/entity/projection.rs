//! `Projection` Domain Entity — rendering intent for a `Wiring`.
//!
//! Aggregate Root identified by [`ProjectionName`]. Carries the rendering
//! intent (which Specification to evaluate + which template / form / plugin
//! to render with). Persisted via the application-layer
//! [`crate::application::projection_registry::ProjectionRegistry`] using the
//! Data Mapper pattern (Fowler PoEAA Ch.10).
//!
//! ## Persistence pattern (SoT)
//!
//! - **PoEAA Registry** ([`crate::application::projection_registry`]) —
//!   named application-layer lookup surface (`register / get / list`).
//!   This is the only entry point CLI / MCP / use cases use to reach a
//!   `Projection`.
//! - **PoEAA Data Mapper** ([`crate::application::projection_mapper`]) —
//!   shape translation between the [`Projection`] Entity (this module)
//!   and `NamedProjection` (the SQLite row mirror DTO). The Registry
//!   owns the Mapper bridge — persona-wire takes the **narrow** reading
//!   of Fowler's Mapper class and does not split out a separate
//!   `Mapper<Dto, Entity>` trait until a second parallel mapper exists.
//! - **DDD Repository** — **not adopted.** A Domain Port trait would
//!   collapse the application-layer Registry into a pass-through; the
//!   PoEAA Registry stance is intentional. See
//!   [`crate::application::projection_registry`] module docs for the
//!   recorded decision.
//!
//! ## Invariants
//!
//! - [`ProjectionName`] / [`SpecName`] / [`ProjectionTemplate`] — non-empty.
//! - [`TargetForm`] — value domain enforced by the enum itself.
//! - [`PluginDispatch`] — `Default` (= framework defaults) or `Custom { engine,
//!   kind, config }` with non-empty `engine` / `kind`. The 3 Optional-field
//!   shape used at the persistence boundary collapses to these two states;
//!   illegal combinations (engine only / kind only) are rejected at the
//!   mapper boundary.
//!
//! Cross-aggregate referential integrity (the [`SpecName`] actually resolving
//! against a `SpecRegistry` row) is **not** enforced here — that is a
//! soft reference handled by `wire_render` / `wire_doctor` at use-case time.
//!
//! ## Vernon IDDD Rule 3 (Identity-by-Name)
//!
//! [`SpecName`] is the Identity Value Object that lets `Projection` reference
//! the `Specification` aggregate by name only (no aggregate-to-aggregate
//! pointer). The legacy type alias [`SpecRef`] is preserved as a re-export
//! for back-compat — new code should prefer `SpecName`.

use std::fmt;

use serde::{Deserialize, Serialize};

use crate::domain::error::{DomainError, WireResult};

// -- ProjectionName ----------------------------------------------------------

/// Projection identifier Value Object. Non-empty.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ProjectionName(String);

impl ProjectionName {
    pub fn new(value: impl Into<String>) -> WireResult<Self> {
        let s = value.into();
        if s.is_empty() {
            return Err(
                DomainError::InvalidProjection("projection name must not be empty".into()).into(),
            );
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProjectionName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for ProjectionName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<ProjectionName> for String {
    fn from(value: ProjectionName) -> Self {
        value.0
    }
}

impl TryFrom<String> for ProjectionName {
    type Error = crate::domain::error::WireError;

    fn try_from(value: String) -> WireResult<Self> {
        Self::new(value)
    }
}

impl TryFrom<&str> for ProjectionName {
    type Error = crate::domain::error::WireError;

    fn try_from(value: &str) -> WireResult<Self> {
        Self::new(value.to_owned())
    }
}

impl<'de> Deserialize<'de> for ProjectionName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

// -- SpecName ----------------------------------------------------------------

/// Identity Value Object for a registered `Specification`. Non-empty.
///
/// Vernon IDDD Rule 3 (Reference Other Aggregates by Identity Only) — the
/// `Projection` aggregate references the `Specification` aggregate solely by
/// this typed name. The `SpecRegistry` lookup happens at use-case time; this
/// VO only carries the typed name and its non-empty invariant.
///
/// The legacy alias [`SpecRef`] is preserved as a `pub type` re-export for
/// back-compat — new code should prefer `SpecName`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct SpecName(String);

impl SpecName {
    pub fn new(value: impl Into<String>) -> WireResult<Self> {
        let s = value.into();
        if s.is_empty() {
            return Err(
                DomainError::InvalidProjection("spec_name must not be empty".into()).into(),
            );
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Back-compat alias for [`SpecName`]. Kept so external crates (and the
/// `spec_ref` wire/SQL field name) can refer to the same VO without churn.
/// New code should use `SpecName` directly.
pub type SpecRef = SpecName;

impl fmt::Display for SpecName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for SpecName {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<SpecName> for String {
    fn from(value: SpecName) -> Self {
        value.0
    }
}

impl TryFrom<String> for SpecName {
    type Error = crate::domain::error::WireError;

    fn try_from(value: String) -> WireResult<Self> {
        Self::new(value)
    }
}

impl TryFrom<&str> for SpecName {
    type Error = crate::domain::error::WireError;

    fn try_from(value: &str) -> WireResult<Self> {
        Self::new(value.to_owned())
    }
}

impl<'de> Deserialize<'de> for SpecName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

// -- ProjectionTemplate ------------------------------------------------------

/// Render template body Value Object. Non-empty.
///
/// Template grammar (mustache-like / handlebars) is owned by the
/// infrastructure rendering layer; this VO only enforces non-emptiness.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ProjectionTemplate(String);

impl ProjectionTemplate {
    pub fn new(value: impl Into<String>) -> WireResult<Self> {
        let s = value.into();
        if s.is_empty() {
            return Err(DomainError::InvalidProjection("template must not be empty".into()).into());
        }
        Ok(Self(s))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ProjectionTemplate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl AsRef<str> for ProjectionTemplate {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl From<ProjectionTemplate> for String {
    fn from(value: ProjectionTemplate) -> Self {
        value.0
    }
}

impl TryFrom<String> for ProjectionTemplate {
    type Error = crate::domain::error::WireError;

    fn try_from(value: String) -> WireResult<Self> {
        Self::new(value)
    }
}

impl TryFrom<&str> for ProjectionTemplate {
    type Error = crate::domain::error::WireError;

    fn try_from(value: &str) -> WireResult<Self> {
        Self::new(value.to_owned())
    }
}

impl<'de> Deserialize<'de> for ProjectionTemplate {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = String::deserialize(deserializer)?;
        Self::new(raw).map_err(serde::de::Error::custom)
    }
}

// -- TargetForm --------------------------------------------------------------

/// Render output form. Domain vocabulary — moved from `application` so
/// `Projection` Entity has no application-layer dependency.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum TargetForm {
    Prompt,
    Markdown,
    Json,
    Ascii,
}

impl TargetForm {
    pub fn as_str(self) -> &'static str {
        match self {
            TargetForm::Prompt => "prompt",
            TargetForm::Markdown => "markdown",
            TargetForm::Json => "json",
            TargetForm::Ascii => "ascii",
        }
    }

    pub fn parse(s: &str) -> WireResult<Self> {
        match s {
            "prompt" => Ok(TargetForm::Prompt),
            "markdown" => Ok(TargetForm::Markdown),
            "json" => Ok(TargetForm::Json),
            "ascii" => Ok(TargetForm::Ascii),
            other => {
                Err(DomainError::InvalidTargetForm(format!("unknown target_form: {other}")).into())
            }
        }
    }
}

impl fmt::Display for TargetForm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

// -- PluginDispatch ----------------------------------------------------------

/// Plugin dispatch hints, modelled to eliminate illegal states at the type
/// level (Yaron Minsky / Jane Street, "Make Illegal States Unrepresentable").
///
/// The persistence shape carries 3 Optional fields (`template_engine` /
/// `projection_kind` / `projection_config`); only two combinations have
/// meaning — all-`None` (framework defaults) and engine+kind both set
/// (explicit Custom dispatch). The remaining 6 combinations are illegal
/// and rejected at the mapper boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "dispatch")]
pub enum PluginDispatch {
    /// Use framework defaults (`handlebars` + `static` + null config).
    Default,
    /// Explicit plugin dispatch. `engine` / `kind` are non-empty; `config`
    /// is plugin-opaque and validated only at the lookup contract.
    Custom {
        engine: String,
        kind: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        config: Option<serde_json::Value>,
    },
}

impl PluginDispatch {
    pub const fn default_dispatch() -> Self {
        Self::Default
    }

    /// Build a `Custom` dispatch, validating `engine` / `kind` non-empty.
    pub fn custom(
        engine: impl Into<String>,
        kind: impl Into<String>,
        config: Option<serde_json::Value>,
    ) -> WireResult<Self> {
        let engine = engine.into();
        let kind = kind.into();
        if engine.is_empty() {
            return Err(DomainError::InvalidProjection(
                "PluginDispatch::Custom.engine must not be empty".into(),
            )
            .into());
        }
        if kind.is_empty() {
            return Err(DomainError::InvalidProjection(
                "PluginDispatch::Custom.kind must not be empty".into(),
            )
            .into());
        }
        Ok(Self::Custom {
            engine,
            kind,
            config,
        })
    }

    /// Mapper-side helper: rebuild from a persistence triple of `Option`s.
    /// Rejects the 6 illegal combinations (engine-only, kind-only, etc.).
    pub fn from_optional_parts(
        engine: Option<String>,
        kind: Option<String>,
        config: Option<serde_json::Value>,
    ) -> WireResult<Self> {
        match (engine, kind, config) {
            (None, None, None) => Ok(Self::Default),
            (Some(e), Some(k), cfg) => Self::custom(e, k, cfg),
            (None, None, Some(_)) => Err(DomainError::InvalidProjection(
                "projection_config present without template_engine + projection_kind".into(),
            )
            .into()),
            (Some(_), None, _) => Err(DomainError::InvalidProjection(
                "template_engine present without projection_kind".into(),
            )
            .into()),
            (None, Some(_), _) => Err(DomainError::InvalidProjection(
                "projection_kind present without template_engine".into(),
            )
            .into()),
        }
    }

    /// Mapper-side helper: project back to the persistence triple.
    pub fn to_optional_parts(&self) -> (Option<&str>, Option<&str>, Option<&serde_json::Value>) {
        match self {
            Self::Default => (None, None, None),
            Self::Custom {
                engine,
                kind,
                config,
            } => (Some(engine.as_str()), Some(kind.as_str()), config.as_ref()),
        }
    }
}

// -- Projection (Aggregate Root) ---------------------------------------------

/// Domain Entity for a registered persona-wire projection.
///
/// Constructed via [`Projection::new`] (typed VO args) or
/// [`Projection::from_parts`] (raw string args, applies all VO validations).
/// Immutable — updates are expressed by constructing a new instance and
/// upserting through `ProjectionRegistry`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Projection {
    name: ProjectionName,
    spec_ref: SpecName,
    template: ProjectionTemplate,
    target_form: TargetForm,
    plugin: PluginDispatch,
}

impl Projection {
    pub fn new(
        name: ProjectionName,
        spec_ref: SpecName,
        template: ProjectionTemplate,
        target_form: TargetForm,
        plugin: PluginDispatch,
    ) -> Self {
        Self {
            name,
            spec_ref,
            template,
            target_form,
            plugin,
        }
    }

    /// Convenience constructor: takes raw strings, applies all VO validations.
    pub fn from_parts(
        name: impl Into<String>,
        spec_ref: impl Into<String>,
        template: impl Into<String>,
        target_form: TargetForm,
        plugin: PluginDispatch,
    ) -> WireResult<Self> {
        Ok(Self::new(
            ProjectionName::new(name)?,
            SpecName::new(spec_ref)?,
            ProjectionTemplate::new(template)?,
            target_form,
            plugin,
        ))
    }

    pub fn name(&self) -> &ProjectionName {
        &self.name
    }

    /// Returns the [`SpecName`] this projection references. Method name keeps
    /// `spec_ref` to align with the persistence column / wire field, which
    /// model "this projection holds a reference to a spec by name".
    pub fn spec_ref(&self) -> &SpecName {
        &self.spec_ref
    }

    pub fn template(&self) -> &ProjectionTemplate {
        &self.template
    }

    pub fn target_form(&self) -> TargetForm {
        self.target_form
    }

    pub fn plugin(&self) -> &PluginDispatch {
        &self.plugin
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::error::WireError;

    // -- VO: ProjectionName --------------------------------------------------

    #[test]
    fn projection_name_accepts_valid() {
        let n = ProjectionName::new("_persona_toc").unwrap();
        assert_eq!(n.as_str(), "_persona_toc");
        assert_eq!(n.to_string(), "_persona_toc");
    }

    #[test]
    fn projection_name_rejects_empty() {
        let err = ProjectionName::new("").expect_err("empty must reject");
        assert!(matches!(
            err,
            WireError::Domain(DomainError::InvalidProjection(_))
        ));
    }

    #[test]
    fn projection_name_serde_roundtrip() {
        let n = ProjectionName::new("foo").unwrap();
        let json = serde_json::to_string(&n).unwrap();
        assert_eq!(json, "\"foo\"");
        let back: ProjectionName = serde_json::from_str(&json).unwrap();
        assert_eq!(back, n);
    }

    #[test]
    fn projection_name_serde_rejects_empty() {
        let err = serde_json::from_str::<ProjectionName>("\"\"").expect_err("reject");
        assert!(err.to_string().contains("must not be empty"));
    }

    // -- VO: SpecName --------------------------------------------------------

    #[test]
    fn spec_name_accepts_valid() {
        let r = SpecName::new("active_personas").unwrap();
        assert_eq!(r.as_str(), "active_personas");
    }

    #[test]
    fn spec_name_rejects_empty() {
        let err = SpecName::new("").expect_err("reject");
        assert!(matches!(
            err,
            WireError::Domain(DomainError::InvalidProjection(_))
        ));
    }

    #[test]
    fn spec_name_serde_roundtrip() {
        let r = SpecName::new("s").unwrap();
        let back: SpecName = serde_json::from_str(&serde_json::to_string(&r).unwrap()).unwrap();
        assert_eq!(back, r);
    }

    /// `SpecRef` alias keeps resolving to `SpecName` for back-compat callers.
    #[test]
    fn spec_ref_alias_resolves_to_spec_name() {
        let r: SpecRef = SpecName::new("active_personas").unwrap();
        assert_eq!(r.as_str(), "active_personas");
    }

    // -- VO: ProjectionTemplate ----------------------------------------------

    #[test]
    fn projection_template_accepts_valid() {
        let t = ProjectionTemplate::new("hello {{name}}").unwrap();
        assert_eq!(t.as_str(), "hello {{name}}");
    }

    #[test]
    fn projection_template_rejects_empty() {
        let err = ProjectionTemplate::new("").expect_err("reject");
        assert!(matches!(
            err,
            WireError::Domain(DomainError::InvalidProjection(_))
        ));
    }

    #[test]
    fn projection_name_string_surface_roundtrip() {
        let raw = "active_personas";
        let n = ProjectionName::new(raw).unwrap();
        assert_eq!(n.to_string(), raw); // Display
        assert_eq!(<ProjectionName as AsRef<str>>::as_ref(&n), raw); // AsRef
        let back: String = n.into();
        assert_eq!(back, raw); // From<ProjectionName> for String
    }

    #[test]
    fn spec_name_string_surface_roundtrip() {
        let raw = "active_personas";
        let n = SpecName::new(raw).unwrap();
        assert_eq!(n.to_string(), raw);
        assert_eq!(<SpecName as AsRef<str>>::as_ref(&n), raw);
        let back: String = n.into();
        assert_eq!(back, raw);
    }

    #[test]
    fn projection_template_string_surface_and_serde_roundtrip() {
        let raw = "hello {{persona}}";
        let t = ProjectionTemplate::new(raw).unwrap();
        // string surface
        assert_eq!(t.to_string(), raw);
        assert_eq!(<ProjectionTemplate as AsRef<str>>::as_ref(&t), raw);
        // serde round-trip (constructor + rejects-empty 以外の coverage を補完)
        let json = serde_json::to_string(&t).unwrap();
        let parsed: ProjectionTemplate = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, t);
        // From consume 経路
        let back: String = t.into();
        assert_eq!(back, raw);
    }

    #[test]
    fn projection_template_serde_rejects_empty() {
        let err = serde_json::from_str::<ProjectionTemplate>("\"\"")
            .expect_err("empty must reject through deserialize");
        assert!(err.to_string().contains("template must not be empty"));
    }

    // -- TargetForm ----------------------------------------------------------

    #[test]
    fn target_form_parse_all_variants() {
        assert_eq!(TargetForm::parse("prompt").unwrap(), TargetForm::Prompt);
        assert_eq!(TargetForm::parse("markdown").unwrap(), TargetForm::Markdown);
        assert_eq!(TargetForm::parse("json").unwrap(), TargetForm::Json);
        assert_eq!(TargetForm::parse("ascii").unwrap(), TargetForm::Ascii);
    }

    #[test]
    fn target_form_parse_rejects_unknown() {
        let err = TargetForm::parse("yaml").expect_err("reject");
        assert!(matches!(
            err,
            WireError::Domain(DomainError::InvalidTargetForm(_))
        ));
    }

    #[test]
    fn target_form_as_str_roundtrip() {
        for v in [
            TargetForm::Prompt,
            TargetForm::Markdown,
            TargetForm::Json,
            TargetForm::Ascii,
        ] {
            assert_eq!(TargetForm::parse(v.as_str()).unwrap(), v);
        }
    }

    // -- PluginDispatch ------------------------------------------------------

    #[test]
    fn plugin_dispatch_default() {
        let d = PluginDispatch::default_dispatch();
        assert_eq!(d, PluginDispatch::Default);
        assert_eq!(d.to_optional_parts(), (None, None, None));
    }

    #[test]
    fn plugin_dispatch_custom_validates_non_empty() {
        let err = PluginDispatch::custom("", "static", None).expect_err("engine empty");
        assert!(matches!(
            err,
            WireError::Domain(DomainError::InvalidProjection(_))
        ));
        let err = PluginDispatch::custom("handlebars", "", None).expect_err("kind empty");
        assert!(matches!(
            err,
            WireError::Domain(DomainError::InvalidProjection(_))
        ));
    }

    #[test]
    fn plugin_dispatch_from_optional_parts_default() {
        let d = PluginDispatch::from_optional_parts(None, None, None).unwrap();
        assert_eq!(d, PluginDispatch::Default);
    }

    #[test]
    fn plugin_dispatch_from_optional_parts_custom() {
        let d = PluginDispatch::from_optional_parts(
            Some("handlebars".into()),
            Some("static".into()),
            None,
        )
        .unwrap();
        assert!(matches!(d, PluginDispatch::Custom { .. }));
    }

    #[test]
    fn plugin_dispatch_from_optional_parts_rejects_illegal() {
        // engine only
        assert!(matches!(
            PluginDispatch::from_optional_parts(Some("h".into()), None, None).unwrap_err(),
            WireError::Domain(DomainError::InvalidProjection(_))
        ));
        // kind only
        assert!(matches!(
            PluginDispatch::from_optional_parts(None, Some("s".into()), None).unwrap_err(),
            WireError::Domain(DomainError::InvalidProjection(_))
        ));
        // config without engine + kind
        assert!(matches!(
            PluginDispatch::from_optional_parts(None, None, Some(serde_json::json!({})))
                .unwrap_err(),
            WireError::Domain(DomainError::InvalidProjection(_))
        ));
    }

    #[test]
    fn plugin_dispatch_to_optional_parts_custom() {
        let cfg = serde_json::json!({"k": 1});
        let d = PluginDispatch::custom("handlebars", "llm", Some(cfg.clone())).unwrap();
        let (e, k, c) = d.to_optional_parts();
        assert_eq!(e, Some("handlebars"));
        assert_eq!(k, Some("llm"));
        assert_eq!(c, Some(&cfg));
    }

    #[test]
    fn plugin_dispatch_serde_default_roundtrip() {
        let d = PluginDispatch::Default;
        let json = serde_json::to_string(&d).unwrap();
        let back: PluginDispatch = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);
    }

    #[test]
    fn plugin_dispatch_serde_custom_roundtrip() {
        let d = PluginDispatch::custom("handlebars", "static", None).unwrap();
        let json = serde_json::to_string(&d).unwrap();
        let back: PluginDispatch = serde_json::from_str(&json).unwrap();
        assert_eq!(back, d);
    }

    // -- Projection ----------------------------------------------------------

    #[test]
    fn projection_from_parts_accepts_valid() {
        let p = Projection::from_parts(
            "_persona_toc",
            "active_personas",
            "Active: {{count}}",
            TargetForm::Prompt,
            PluginDispatch::Default,
        )
        .unwrap();
        assert_eq!(p.name().as_str(), "_persona_toc");
        assert_eq!(p.spec_ref().as_str(), "active_personas");
        assert_eq!(p.template().as_str(), "Active: {{count}}");
        assert_eq!(p.target_form(), TargetForm::Prompt);
        assert_eq!(p.plugin(), &PluginDispatch::Default);
    }

    #[test]
    fn projection_from_parts_propagates_vo_errors() {
        let err = Projection::from_parts(
            "",
            "spec",
            "tmpl",
            TargetForm::Prompt,
            PluginDispatch::Default,
        )
        .expect_err("empty name");
        assert!(matches!(
            err,
            WireError::Domain(DomainError::InvalidProjection(_))
        ));

        let err =
            Projection::from_parts("n", "", "tmpl", TargetForm::Prompt, PluginDispatch::Default)
                .expect_err("empty spec_ref");
        assert!(matches!(
            err,
            WireError::Domain(DomainError::InvalidProjection(_))
        ));

        let err = Projection::from_parts("n", "s", "", TargetForm::Prompt, PluginDispatch::Default)
            .expect_err("empty template");
        assert!(matches!(
            err,
            WireError::Domain(DomainError::InvalidProjection(_))
        ));
    }

    #[test]
    fn projection_immutable_equality() {
        let p1 =
            Projection::from_parts("n", "s", "t", TargetForm::Markdown, PluginDispatch::Default)
                .unwrap();
        let p2 =
            Projection::from_parts("n", "s", "t", TargetForm::Markdown, PluginDispatch::Default)
                .unwrap();
        assert_eq!(p1, p2);
    }

    #[test]
    fn projection_new_typed_vo_path_assembles() {
        // Primary path used by mappers: typed VO 直接受け取り、 validation 済みの instance を組む。
        let p = Projection::new(
            ProjectionName::new("toc").unwrap(),
            SpecName::new("active_personas").unwrap(),
            ProjectionTemplate::new("Count: {{n}}").unwrap(),
            TargetForm::Json,
            PluginDispatch::custom("handlebars", "llm", None).unwrap(),
        );
        assert_eq!(p.name().as_str(), "toc");
        assert_eq!(p.spec_ref().as_str(), "active_personas");
        assert_eq!(p.template().as_str(), "Count: {{n}}");
        assert_eq!(p.target_form(), TargetForm::Json);
        assert!(matches!(p.plugin(), PluginDispatch::Custom { .. }));
    }

    #[test]
    fn plugin_dispatch_from_optional_parts_rejects_engine_with_config() {
        // (Some, None, Some): kind 欠落、 config だけ伴う engine。
        let err = PluginDispatch::from_optional_parts(
            Some("handlebars".into()),
            None,
            Some(serde_json::json!({"x": 1})),
        )
        .expect_err("engine + config without kind must reject");
        assert!(matches!(
            err,
            WireError::Domain(DomainError::InvalidProjection(_))
        ));
    }

    #[test]
    fn plugin_dispatch_from_optional_parts_rejects_kind_with_config() {
        // (None, Some, Some): engine 欠落、 config だけ伴う kind。
        let err = PluginDispatch::from_optional_parts(
            None,
            Some("static".into()),
            Some(serde_json::json!({"x": 1})),
        )
        .expect_err("kind + config without engine must reject");
        assert!(matches!(
            err,
            WireError::Domain(DomainError::InvalidProjection(_))
        ));
    }
}
